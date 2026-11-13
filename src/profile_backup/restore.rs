//! Replay a validated restore before opening any live profile stores.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};

use kortty_vault::Vault;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::java_migration::{CONNECTIONS, SchedulerSource};
use super::{
    QueuedRestore, Snapshot, archive, data, java_credential_restore, java_migration, java_scheduler,
};
use crate::ai_manager::backup as ai_backup;
use crate::private_files::{Directory, nonce};
use crate::snippets;
use crate::ssh_key_catalog::{self, Catalog, KeySource};

#[cfg(test)]
mod java_ai_tests;
mod java_jobs;
mod java_keys;
#[cfg(test)]
mod java_scheduler_tests;
mod merge;
#[cfg(test)]
mod tests;

const LEASE: &str = ".profile.lock";
const PENDING: &str = ".pending-backup-restore";
const PENDING_MAGIC: &[u8] = b"kortty restore\x01";
const PENDING_MAGIC_WITH_MODE: &[u8] = b"kortty restore\x02";
const KEEP_EXISTING: u8 = 1;
const JAVA_ADDITIVE: u8 = 2;
const JAVA_PASSWORDS: u8 = 3;
const JAVA_PASSWORDS_COMMITTING: u8 = 4;
const JAVA_CREDENTIALS: u8 = 5;
const JAVA_CREDENTIALS_COMMITTING: u8 = 6;
const JAVA_JOBS: u8 = 7;
const JAVA_JOBS_COMMITTING: u8 = 8;
const VAULT: &str = "vault/vault.bin";
const MAX_PASSWORD_PLAN: usize = 16 * 1024;
const MAX_CREDENTIAL_PLAN: usize = 128 * 1024;
const NONCE_LEN: usize = 32;
const PINS: &str = "ssh/known_hosts";
const JOBS: &str = "jobs.json";
const RECORDINGS: &str = "job-recordings";
const JOB_HISTORY: &str = "job-history";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RestoreMode {
    Replace,
    KeepExisting,
    JavaAdditive,
}

pub(crate) struct ProfileLease {
    lock: File,
    root: PathBuf,
}

impl ProfileLease {
    pub(crate) fn acquire(root: &Path, protect_pins: impl Fn() -> bool) -> io::Result<Self> {
        Self::acquire_with_header_policy(
            root,
            protect_pins,
            snippets::custom_script_headers_allowed,
        )
    }

    fn acquire_with_header_policy(
        root: &Path,
        protect_pins: impl Fn() -> bool,
        allow_headers: impl Fn() -> bool,
    ) -> io::Result<Self> {
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
                apply_pending(&lease.root, &directory, file, &protect_pins, &allow_headers)
                    .map_err(|error| {
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
    snapshot: Snapshot,
    mode: RestoreMode,
    protect_pins: bool,
) -> io::Result<bool> {
    queue_with_header_policy(
        root,
        snapshot,
        mode,
        protect_pins,
        snippets::custom_script_headers_allowed,
    )
}

fn queue_with_header_policy(
    root: &Path,
    mut snapshot: Snapshot,
    mode: RestoreMode,
    protect_pins: bool,
    allow_headers: impl FnOnce() -> bool,
) -> io::Result<bool> {
    let (_, directory) = open_root(root)?;
    check_script_headers(&directory, &snapshot, mode, allow_headers)?;
    match mode {
        RestoreMode::Replace => check_pins(&directory, &snapshot, protect_pins)?,
        RestoreMode::KeepExisting => {
            if protect_pins {
                snapshot.remove(PINS);
            }
        }
        RestoreMode::JavaAdditive => check_java_delta(&snapshot)?,
    }
    let payload = archive::encode_snapshot(&snapshot)?;
    drop(snapshot);
    let id = nonce()?;
    let mut pending = Zeroizing::new(Vec::with_capacity(
        PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN + payload.len(),
    ));
    match mode {
        RestoreMode::Replace => pending.extend_from_slice(PENDING_MAGIC),
        RestoreMode::KeepExisting | RestoreMode::JavaAdditive => {
            pending.extend_from_slice(PENDING_MAGIC_WITH_MODE);
            pending.push(if mode == RestoreMode::KeepExisting {
                KEEP_EXISTING
            } else {
                JAVA_ADDITIVE
            });
        }
    }
    pending.extend_from_slice(id.as_bytes());
    pending.extend_from_slice(&payload);
    directory.publish_copy(PENDING, &pending)?;
    Ok(directory.sync().is_ok())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JavaPasswordPlan {
    vault_before: [u8; 32],
    vault_after: [u8; 32],
    ids: Vec<String>,
    connections: Option<ConnectionRevision>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JavaCredentialPlan {
    vault: Option<VaultRevision>,
    connections: Option<ConnectionRevision>,
    keys: Option<java_keys::Revision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jobs: Option<[u8; 32]>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VaultRevision {
    before: [u8; 32],
    after: [u8; 32],
    ids: Vec<String>,
}

impl From<JavaPasswordPlan> for JavaCredentialPlan {
    fn from(plan: JavaPasswordPlan) -> Self {
        Self {
            vault: Some(VaultRevision {
                before: plan.vault_before,
                after: plan.vault_after,
                ids: plan.ids,
            }),
            connections: plan.connections,
            keys: None,
            jobs: None,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConnectionRevision {
    before: Option<[u8; 32]>,
    after: [u8; 32],
}

#[derive(Clone)]
pub(crate) struct PendingJavaCancellation {
    root: PathBuf,
    id: String,
    stamp: crate::private_files::FileStamp,
}

/// Offers recovery for a transactional Java restore that has not started publishing.
pub(crate) fn pending_java_cancellation(
    root: &Path,
) -> io::Result<Option<PendingJavaCancellation>> {
    let (root, directory) = open_root(root)?;
    let lock = directory.open_lock(LEASE)?;
    lock.try_lock().map_err(lock_error)?;
    Ok(
        unstarted_java_id(&directory)?.map(|(id, stamp)| PendingJavaCancellation {
            root,
            id,
            stamp,
        }),
    )
}

pub(crate) fn discard_pending_java(cancellation: &PendingJavaCancellation) -> io::Result<()> {
    let (_, directory) = open_root(&cancellation.root)?;
    let lock = directory.open_lock(LEASE)?;
    lock.try_lock().map_err(lock_error)?;
    let current = unstarted_java_id(&directory)?;
    if !current.is_some_and(|(id, stamp)| id == cancellation.id && stamp == cancellation.stamp) {
        return Err(io::Error::other(
            "The pending Java import changed or started applying.",
        ));
    }
    directory.remove_unchanged(PENDING, &cancellation.stamp)?;
    directory.sync()
}

fn unstarted_java_id(
    directory: &Directory,
) -> io::Result<Option<(String, crate::private_files::FileStamp)>> {
    let Some(file) = missing(directory.open_private_file(PENDING))? else {
        return Ok(None);
    };
    let stamp = crate::private_files::FileStamp::read(&file)?;
    let mut header = [0; PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN];
    match (&file).read_exact(&mut header) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }
    stamp.require_unchanged(&file)?;
    let Some((&mode, body)) = header
        .strip_prefix(PENDING_MAGIC_WITH_MODE)
        .and_then(|body| body.split_first())
    else {
        return Ok(None);
    };
    if !matches!(mode, JAVA_PASSWORDS | JAVA_CREDENTIALS | JAVA_JOBS) {
        return Ok(None);
    }
    pending_body(body).map(|(id, _)| Some((id.to_owned(), stamp)))
}

pub(super) fn read_java_connections(root: &Path) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let (_, directory) = open_root(root)?;
    read_connections(&directory)
}

fn read_connections(directory: &Directory) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    missing(directory.open_file(CONNECTIONS))?
        .map(|file| read_bounded(file, data::file_limit(CONNECTIONS)))
        .transpose()
}

pub(super) fn queue_java_transaction(
    root: &Path,
    mut snapshot: Snapshot,
    prepared: Option<java_credential_restore::Prepared>,
    scheduler: Option<SchedulerSource>,
) -> io::Result<QueuedRestore> {
    let (_, directory) = open_root(root)?;
    if !snapshot.is_empty() {
        check_java_delta(&snapshot)?;
    }
    data::validate(&snapshot)?;
    let transactional_credentials = prepared.is_some();
    let mut prepared = prepared.unwrap_or_default();
    let mut scheduler_notes = Vec::new();
    let jobs = scheduler
        .map(|source| {
            java_jobs::stage(
                &directory,
                &snapshot,
                &mut prepared.connections,
                source,
                &mut scheduler_notes,
            )
        })
        .transpose()?
        .flatten();
    let job_revision = jobs
        .as_deref()
        .map(|bytes| java_credential_restore::digest(bytes));
    if let Some(jobs) = jobs {
        snapshot.insert(JOBS.into(), jobs);
    } else if !transactional_credentials {
        if snapshot.is_empty() {
            return Err(invalid(
                "No Java Scheduler jobs could be matched to this profile.",
            ));
        }
        let durable = queue_with_mode(root, snapshot, RestoreMode::JavaAdditive, false)?;
        return Ok(QueuedRestore {
            durable,
            scheduler_notes,
        });
    }
    let credential_ids = prepared
        .vault
        .as_ref()
        .map_or(&[][..], |vault| vault.ids.as_slice());
    let keys = prepared
        .keys
        .map(|keys| java_keys::stage(&directory, &mut snapshot, keys, credential_ids))
        .transpose()?;
    let key_ids = keys
        .as_ref()
        .map_or_else(Vec::new, java_keys::Revision::ids);
    let connections = if let Some(connections) = prepared.connections {
        let after = java_credential_restore::digest(&connections.contents);
        let plan = ConnectionRevision {
            before: connections.before,
            after,
        };
        require_connection_revision(&directory, &plan, false)?;
        let policy = crate::clipboard_policy::ClipboardPolicy::load();
        crate::connection_store::import::check_java_connection_routes(
            &connections.contents,
            credential_ids,
            &key_ids,
            policy.server_access(),
        )?;
        snapshot.insert(CONNECTIONS.into(), connections.contents);
        Some(plan)
    } else {
        None
    };
    let vault = if let Some(vault) = prepared.vault {
        let revision = VaultRevision {
            before: vault.before,
            after: vault.after,
            ids: vault.ids,
        };
        revision.validate(&vault.snapshot)?;
        directory.subdirectory("vault", false)?;
        // An unlocked desktop vault owns its lease until shutdown.
        let current = Vault::encrypted_envelope(crate::profile::vault_path_at(root))
            .map_err(io::Error::other)?;
        if java_credential_restore::digest(&current) != revision.before {
            return Err(io::Error::other(
                "The destination vault changed after the Java credential review.",
            ));
        }
        snapshot.insert(VAULT.into(), vault.snapshot);
        Some(revision)
    } else {
        None
    };
    let (mode, encoded) = if job_revision.is_some() || keys.is_some() {
        let plan = JavaCredentialPlan {
            vault,
            connections,
            keys,
            jobs: job_revision,
        };
        let mode = if job_revision.is_some() {
            JAVA_JOBS
        } else {
            JAVA_CREDENTIALS
        };
        (mode, serde_json::to_vec(&plan))
    } else {
        let vault = vault.ok_or_else(|| invalid("The Java credential review is empty."))?;
        let plan = JavaPasswordPlan {
            vault_before: vault.before,
            vault_after: vault.after,
            ids: vault.ids,
            connections,
        };
        (JAVA_PASSWORDS, serde_json::to_vec(&plan))
    };
    let encoded = encoded.map_err(io::Error::other)?;
    if encoded.len() > java_plan_limit(mode) {
        return Err(invalid("Java backup restore exceeds its review limit."));
    }
    let payload = archive::encode_snapshot(&snapshot)?;
    let id = nonce()?;
    directory.publish_with(PENDING, |file| {
        file.write_all(PENDING_MAGIC_WITH_MODE)?;
        file.write_all(&[mode])?;
        file.write_all(id.as_bytes())?;
        file.write_all(&(encoded.len() as u32).to_le_bytes())?;
        file.write_all(&encoded)?;
        file.write_all(&payload)
    })?;
    Ok(QueuedRestore {
        durable: directory.sync().is_ok(),
        scheduler_notes,
    })
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
    allow_headers: &impl Fn() -> bool,
) -> io::Result<()> {
    let pending = read_bounded(
        file,
        PENDING_MAGIC_WITH_MODE.len()
            + 1
            + NONCE_LEN
            + 4
            + MAX_CREDENTIAL_PLAN
            + archive::MAX_PAYLOAD_BYTES,
    )?;
    if let Some((&mode, body)) = pending
        .strip_prefix(PENDING_MAGIC_WITH_MODE)
        .and_then(|body| body.split_first())
        && matches!(
            mode,
            JAVA_PASSWORDS
                | JAVA_PASSWORDS_COMMITTING
                | JAVA_CREDENTIALS
                | JAVA_CREDENTIALS_COMMITTING
                | JAVA_JOBS
                | JAVA_JOBS_COMMITTING
        )
    {
        let (_, payload) = pending_body(body)?;
        apply_java_transaction(root, directory, mode, payload, body, allow_headers)?;
        directory.remove_copy(PENDING)?;
        let _ = directory.sync();
        return Ok(());
    }
    let (mode, body) = if let Some(body) = pending.strip_prefix(PENDING_MAGIC) {
        (RestoreMode::Replace, body)
    } else if let Some((flag, body)) = pending
        .strip_prefix(PENDING_MAGIC_WITH_MODE)
        .and_then(|body| body.split_first())
    {
        let mode = match *flag {
            KEEP_EXISTING => RestoreMode::KeepExisting,
            JAVA_ADDITIVE => RestoreMode::JavaAdditive,
            _ => return Err(invalid("Invalid pending backup restore mode.")),
        };
        (mode, body)
    } else {
        return Err(invalid("Invalid pending backup restore mode."));
    };
    let (id, payload) = pending_body(body)?;
    let id = id.to_owned();
    let mut snapshot = archive::decode_snapshot(payload)?;
    drop(pending);
    data::validate(&snapshot)?;
    if mode == RestoreMode::JavaAdditive {
        check_java_delta(&snapshot)?;
    }
    data::ai::prepare(&mut snapshot)?;
    if let Some(jobs) = snapshot.get_mut(JOBS) {
        *jobs = Zeroizing::new(data::portable_jobs(jobs)?);
    }
    let _snippet_lock = if snapshot.contains_key(snippets::FILE_NAME)
        || mode == RestoreMode::Replace && directory.has_entry(snippets::FILE_NAME)?
    {
        Some(snippets::StoreLock::acquire_for_restore(root)?)
    } else {
        None
    };
    check_script_headers(directory, &snapshot, mode, allow_headers)?;
    if mode != RestoreMode::JavaAdditive {
        crate::profile::session::Store::purge_saved_output(root)?;
    }
    match mode {
        RestoreMode::Replace => {
            apply_replace(root, directory, &id, &snapshot, protect_pins, allow_headers)?
        }
        RestoreMode::KeepExisting => {
            merge::apply(directory, &snapshot, protect_pins, allow_headers)?
        }
        RestoreMode::JavaAdditive => apply_java_delta(directory, &snapshot, allow_headers)?,
    }
    directory.remove_copy(PENDING)?;
    // A repeated marker after a power loss safely replays the same snapshot.
    let _ = directory.sync();
    Ok(())
}

fn pending_body(body: &[u8]) -> io::Result<(&str, &[u8])> {
    let (id, payload) = body
        .split_at_checked(NONCE_LEN)
        .ok_or_else(|| invalid("Invalid pending backup restore."))?;
    let id = std::str::from_utf8(id)
        .ok()
        .filter(|id| {
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(|| invalid("Invalid pending backup restore identifier."))?;
    Ok((id, payload))
}

fn java_plan_limit(mode: u8) -> usize {
    if matches!(mode, JAVA_PASSWORDS | JAVA_PASSWORDS_COMMITTING) {
        MAX_PASSWORD_PLAN
    } else {
        MAX_CREDENTIAL_PLAN
    }
}

fn apply_java_transaction(
    root: &Path,
    directory: &Directory,
    mode: u8,
    payload: &[u8],
    body: &[u8],
    allow_headers: &impl Fn() -> bool,
) -> io::Result<()> {
    let unstarted = matches!(mode, JAVA_PASSWORDS | JAVA_CREDENTIALS | JAVA_JOBS);
    let (size, payload) = payload
        .split_first_chunk::<4>()
        .ok_or_else(|| invalid("Invalid Java credential restore plan."))?;
    let size = u32::from_le_bytes(*size) as usize;
    let (encoded, payload) = payload
        .split_at_checked(size)
        .filter(|_| size <= java_plan_limit(mode))
        .ok_or_else(|| invalid("Invalid Java credential restore plan size."))?;
    let plan: JavaCredentialPlan = if matches!(mode, JAVA_PASSWORDS | JAVA_PASSWORDS_COMMITTING) {
        serde_json::from_slice::<JavaPasswordPlan>(encoded).map(Into::into)
    } else {
        serde_json::from_slice::<JavaCredentialPlan>(encoded)
    }
    .map_err(|_| invalid("Invalid Java credential restore plan."))?;
    if matches!(mode, JAVA_CREDENTIALS | JAVA_CREDENTIALS_COMMITTING) && plan.keys.is_none() {
        return Err(invalid(
            "Java credential restore is missing its managed keys.",
        ));
    }
    let mut snapshot = archive::decode_snapshot(payload)?;
    let incoming_jobs = if matches!(mode, JAVA_JOBS | JAVA_JOBS_COMMITTING) {
        let bytes = snapshot
            .remove(JOBS)
            .filter(|bytes| plan.jobs == Some(java_credential_restore::digest(bytes)))
            .ok_or_else(|| invalid("Invalid reviewed Java Scheduler jobs."))?;
        Some(java_scheduler::decode(&bytes)?)
    } else {
        if plan.jobs.is_some() {
            return Err(invalid("Unexpected Java Scheduler restore plan."));
        }
        None
    };
    let vault = plan
        .vault
        .as_ref()
        .map(|revision| {
            let vault = snapshot
                .remove(VAULT)
                .ok_or_else(|| invalid("Java credential restore is missing its sealed vault."))?;
            revision.require_current(root, directory, &vault, !unstarted)?;
            Ok::<_, io::Error>(vault)
        })
        .transpose()?;
    let credential_ids = plan
        .vault
        .as_ref()
        .map_or(&[][..], |vault| vault.ids.as_slice());
    let _connection_lock = (plan.connections.is_some() || snapshot.contains_key(CONNECTIONS))
        .then(|| {
            crate::connection_store::StoreLock::acquire(&crate::profile::connections_path_at(root))
        })
        .transpose()?;
    let _key_lock = plan
        .keys
        .as_ref()
        .map(|_| ssh_key_catalog::lock_for_restore(directory))
        .transpose()?;
    let keys = plan
        .keys
        .map(|revision| {
            java_keys::Pending::read(
                directory,
                &mut snapshot,
                revision,
                credential_ids,
                !unstarted,
            )
        })
        .transpose()?;
    let key_ids = keys.as_ref().map_or_else(Vec::new, java_keys::Pending::ids);
    let connections = if let Some(revision) = plan.connections {
        let contents = snapshot
            .remove(CONNECTIONS)
            .filter(|bytes| java_credential_restore::digest(bytes) == revision.after)
            .ok_or_else(|| invalid("Invalid Java credential restore connections."))?;
        if unstarted {
            let policy = crate::clipboard_policy::ClipboardPolicy::load();
            crate::connection_store::import::check_java_connection_routes(
                &contents,
                credential_ids,
                &key_ids,
                policy.server_access(),
            )?;
        }
        // Once publication begins, replay settles the same saved records.
        // Launches still require the current server policy.
        require_connection_revision(directory, &revision, !unstarted)?;
        Some((revision, contents))
    } else {
        None
    };
    if !snapshot.is_empty() {
        check_java_delta(&snapshot)?;
    }
    data::validate(&snapshot)?;
    let _snippet_lock = snapshot
        .contains_key(snippets::FILE_NAME)
        .then(|| snippets::StoreLock::acquire_for_restore(root))
        .transpose()?;
    let _ai_lock = snapshot
        .contains_key(ai_backup::SETTINGS)
        .then(|| ai_backup::lock_for_restore(directory))
        .transpose()?;
    let updates = prepare_java_updates(directory, &snapshot, allow_headers)?;
    let jobs = incoming_jobs
        .map(|incoming| {
            let candidate = connections.as_ref().ok_or_else(|| {
                invalid("Java Scheduler restore is missing its connection catalogue.")
            })?;
            let policy = if unstarted {
                crate::clipboard_policy::ClipboardPolicy::load()
            } else {
                // Once publication starts, complete the disabled definitions on replay.
                // Their launch still checks current policy.
                crate::clipboard_policy::ClipboardPolicy::default()
            };
            java_jobs::Pending::prepare(
                root,
                directory,
                &snapshot,
                &updates,
                &candidate.1,
                incoming,
                &policy,
            )
        })
        .transpose()?;
    if unstarted {
        let committing = match mode {
            JAVA_PASSWORDS => JAVA_PASSWORDS_COMMITTING,
            JAVA_JOBS => JAVA_JOBS_COMMITTING,
            _ => JAVA_CREDENTIALS_COMMITTING,
        };
        // Flush the intent before changing any credential data or dependent catalogue.
        // A failed restart can only discard a plan that never reached this point.
        directory.replace_with(
            PENDING,
            |file| {
                file.write_all(PENDING_MAGIC_WITH_MODE)?;
                file.write_all(&[committing])?;
                file.write_all(body)
            },
            || Ok(()),
        )?;
        directory.sync()?;
    }
    // The profile lease excludes all desktop instances. The vault lock and
    // pre-rename checks additionally protect the individually published files.
    if let Some((revision, vault)) = plan.vault.as_ref().zip(vault.as_ref()) {
        Vault::apply_prepared_additions(
            crate::profile::vault_path_at(root),
            &revision.before,
            &revision.after,
            vault,
        )
        .map_err(io::Error::other)?;
    }
    if let Some(keys) = keys {
        keys.publish(directory)?;
    }
    if let Some((revision, contents)) = connections
        && !require_connection_revision(directory, &revision, true)?
    {
        let write = |file: &mut File| file.write_all(&contents);
        let check = || require_connection_revision(directory, &revision, false).map(|_| ());
        if revision.before.is_none() {
            directory.publish_checked(CONNECTIONS, write, check)?;
        } else {
            directory.replace_with(CONNECTIONS, write, check)?;
        }
        directory.sync()?;
    }
    publish_java_updates(directory, updates)?;
    if let Some(jobs) = jobs {
        jobs.publish(directory)?;
    }
    Ok(())
}

impl VaultRevision {
    fn validate(&self, sealed: &[u8]) -> io::Result<()> {
        let ids: BTreeSet<_> = self.ids.iter().collect();
        if ids.is_empty()
            || ids.len() != self.ids.len()
            || ids.len() > 512
            || ids.iter().any(|id| {
                id.len() != 32
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(invalid("Invalid Java credential restore vault IDs."));
        }
        if sealed.len() > data::file_limit(VAULT) {
            return Err(invalid(
                "The prepared Java credential vault exceeds its size limit.",
            ));
        }
        if java_credential_restore::digest(sealed) != self.after || self.before == self.after {
            return Err(invalid("Invalid prepared Java credential vault revision."));
        }
        Vault::validate_backup_envelope(sealed).map_err(io::Error::other)
    }

    fn require_current(
        &self,
        root: &Path,
        directory: &Directory,
        sealed: &[u8],
        allow_after: bool,
    ) -> io::Result<()> {
        self.validate(sealed)?;
        directory.subdirectory("vault", false)?;
        let state = Vault::verify_prepared_additions(
            crate::profile::vault_path_at(root),
            &self.before,
            &self.after,
            sealed,
        )
        .map_err(io::Error::other)?;
        if !allow_after && state != kortty_vault::PreparedState::Before {
            return Err(invalid(
                "The unstarted Java import already has a changed vault.",
            ));
        }
        Ok(())
    }
}

/// Returns whether the exact candidate is already installed during replay.
fn require_connection_revision(
    directory: &Directory,
    plan: &ConnectionRevision,
    allow_after: bool,
) -> io::Result<bool> {
    let current = read_connections(directory)?;
    let actual = current
        .as_deref()
        .map(|bytes| java_credential_restore::digest(bytes));
    if allow_after && actual == Some(plan.after) {
        return Ok(true);
    }
    if actual == plan.before {
        return Ok(false);
    }
    Err(io::Error::other(
        "Saved connections changed after the Java backup review.",
    ))
}

fn check_java_delta(snapshot: &Snapshot) -> io::Result<()> {
    if snapshot.is_empty()
        || snapshot.keys().any(|path| {
            !java_migration::is_catalogue(path)
                && data::ai::chat_folder(path).is_none()
                && !path
                    .strip_prefix("projects/")
                    .is_some_and(crate::profile::projects::is_backup_name)
        })
    {
        return Err(invalid("Invalid pending Java backup data."));
    }
    for (path, bytes) in snapshot
        .iter()
        .filter(|(path, _)| data::ai::chat_folder(path).is_some())
    {
        data::ai::validate_java_chat(path, bytes).map_err(|error| path_error(path, error))?;
    }
    Ok(())
}

fn apply_java_delta(
    directory: &Directory,
    snapshot: &Snapshot,
    allow_headers: &impl Fn() -> bool,
) -> io::Result<()> {
    let _ai_lock = snapshot
        .contains_key(ai_backup::SETTINGS)
        .then(|| ai_backup::lock_for_restore(directory))
        .transpose()?;
    let updates = prepare_java_updates(directory, snapshot, allow_headers)?;
    publish_java_updates(directory, updates)
}

#[derive(Default)]
struct JavaUpdates<'a> {
    catalogues: Vec<(&'a str, Zeroizing<Vec<u8>>)>,
    additions: Vec<(&'a str, &'a [u8])>,
    chat_parents: BTreeMap<&'a str, &'a str>,
}

fn prepare_java_updates<'a>(
    directory: &Directory,
    snapshot: &'a Snapshot,
    allow_headers: &impl Fn() -> bool,
) -> io::Result<JavaUpdates<'a>> {
    let mut updates = JavaUpdates::default();
    let mut ai = data::ai::Restore::default();
    for (path, incoming) in snapshot {
        if java_migration::is_catalogue(path) {
            let local = missing(directory.open_file(path))?
                .map(|file| read_bounded(file, data::file_limit(path)))
                .transpose()?;
            if let Some(bytes) = java_migration::merge_catalogue(
                path,
                local.as_deref().map(Vec::as_slice),
                incoming,
                allow_headers,
            )
            .map_err(|error| path_error(path, error))?
            {
                updates.catalogues.push((path, Zeroizing::new(bytes)));
            }
            continue;
        }
        let exists = with_target(directory, path, false, Directory::has_entry)
            .map_err(|error| path_error(path, error))?
            .unwrap_or(false);
        if let Some(folder) = data::ai::chat_folder(path) {
            updates.chat_parents.entry(folder).or_insert(path);
            if !exists && !ai.may_install(directory, snapshot, path)? {
                continue;
            }
        }
        if !exists {
            updates.additions.push((path, incoming));
        }
    }
    Ok(updates)
}

fn publish_java_updates(directory: &Directory, updates: JavaUpdates<'_>) -> io::Result<()> {
    for (path, bytes) in updates.catalogues {
        if path == ai_backup::SETTINGS {
            ai_backup::publish_java_https(directory, &bytes)
        } else {
            directory.replace_file(path, &bytes)
        }
        .map_err(|error| path_error(path, error))?;
    }
    for (path, bytes) in updates.additions {
        merge::publish(directory, path, bytes)?;
    }
    for path in updates.chat_parents.into_values() {
        merge::sync_parent(directory, path)?;
    }
    directory.sync()
}

fn apply_replace(
    root: &Path,
    directory: &Directory,
    id: &str,
    snapshot: &Snapshot,
    protect_pins: &impl Fn() -> bool,
    allow_headers: &impl Fn() -> bool,
) -> io::Result<()> {
    check_pins(directory, snapshot, protect_pins())?;

    let obsolete_backgrounds = data::installed_background_ids(directory)?.unwrap_or_default();
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
    let mut ai = data::ai::Restore::default();
    for (path, data) in snapshot {
        if path == PINS && protect_pins() {
            check_pins(directory, snapshot, true)?;
            continue;
        }
        if !ai.may_install(directory, snapshot, path)? {
            continue;
        }
        if self::data::ai::only_missing(path) {
            merge::publish(directory, path, data)?;
            continue;
        }
        let rebound = ai.rebind(directory, snapshot, path)?;
        let data = rebound.as_ref().unwrap_or(data);
        with_target(directory, path, true, |parent, name| {
            if path == snippets::FILE_NAME && !allow_headers() {
                snippets::check_restore_headers(Some(directory), data)?;
            }
            parent.replace_file(name, data)?;
            parent.sync()
        })
        .map_err(|error| path_error(path, error))?;
    }
    // Make the new settings durable before retiring any old image. Ordinary
    // Settings saves share the profile lease and never remove these copies.
    directory.sync()?;
    for id in obsolete_backgrounds {
        if !snapshot.contains_key(&id.relative_path())
            && let Err(error) = crate::terminal_view::background_image::retire(root, id)
        {
            eprintln!("kortty: could not retire a terminal background: {error}");
        }
    }
    Ok(())
}

pub(super) fn with_target<T>(
    directory: &Directory,
    path: &str,
    create: bool,
    operation: impl FnOnce(&Directory, &str) -> io::Result<T>,
) -> io::Result<Option<T>> {
    let Some((parents, name)) = path.rsplit_once('/') else {
        return operation(directory, path).map(Some);
    };
    let mut child = None;
    for parent in parents.split('/') {
        let current = child.as_ref().unwrap_or(directory);
        let result = if parent == "ssh-keys"
            || parent == crate::terminal_view::background_image::DIRECTORY
            || cfg!(unix)
            || data::ai::requires_private_parent(path)
        {
            current.private_child(parent, create)
        } else {
            // Existing Windows memory and profile directories may inherit ACLs.
            // Their restored files still use the private file creation path.
            current.subdirectory(parent, create)
        };
        child = if create {
            Some(result?)
        } else {
            missing(result)?
        };
        if child.is_none() {
            return Ok(None);
        }
    }
    operation(child.as_ref().expect("target has a parent"), name).map(Some)
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

fn check_script_headers(
    directory: &Directory,
    snapshot: &Snapshot,
    mode: RestoreMode,
    allow_headers: impl FnOnce() -> bool,
) -> io::Result<()> {
    if let Some(incoming) = snapshot.get(snippets::FILE_NAME)
        && mode != RestoreMode::JavaAdditive
        && !(mode == RestoreMode::KeepExisting && directory.has_entry(snippets::FILE_NAME)?)
        && !allow_headers()
    {
        snippets::check_restore_headers(
            (mode == RestoreMode::Replace).then_some(directory),
            incoming,
        )?;
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
