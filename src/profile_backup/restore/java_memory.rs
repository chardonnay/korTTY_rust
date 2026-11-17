//! Stage connection-scoped Java Memory against the exact imported catalogue.

use std::collections::BTreeSet;
use std::io;

use sha2::{Digest, Sha256};

use super::{Directory, Snapshot, archive, data, invalid, java_credential_restore, merge};
use crate::ai_manager::backup as ai;
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::Store;
use crate::connection_store::import::java_connection_candidate;
use crate::profile_backup::java_migration::memory::{self, ARCHIVE, MappedScopes};

mod review;
pub(crate) use review::{Review, Target};

pub(super) fn stage(
    source: memory::Source,
    snapshot: &Snapshot,
    catalogue: Option<&java_credential_restore::Connections>,
    notes: &mut Vec<String>,
) -> io::Result<Snapshot> {
    let total = source.files.len();
    let policy = ClipboardPolicy::load();
    let Some(catalogue) = catalogue.filter(|_| data::ai::policy_allows(ARCHIVE, &policy)) else {
        notes.push(format!(
            "Connection AI Memory files omitted because no new profiles matched or managed policy prevents import ({total})"
        ));
        return Ok(Snapshot::new());
    };
    let candidate = java_connection_candidate(&catalogue.contents)?;
    let reviewed = source
        .review
        .as_ref()
        .map(|review| review.bindings(catalogue.before))
        .transpose()?
        .unwrap_or_default();
    let scopes = MappedScopes::read(&source.connections, |original_id| {
        reviewed
            .get(original_id)
            .or_else(|| catalogue.memory_ids.get(original_id))
            .copied()
            .filter(|&id| eligible(&candidate, id, &policy))
    })?;
    let mut reserve = archive::remaining_capacity(snapshot)?;
    let mut files = Snapshot::new();
    for (path, bytes) in source.files {
        if let Some((target, converted)) = memory::bind(&path, bytes, &scopes)
            && reserve(&target, converted.len())
        {
            files.insert(target, converted);
        }
    }
    if total > files.len() {
        notes.push(format!(
            "Connection AI Memory files omitted because their scopes could not be safely imported ({})",
            total - files.len()
        ));
    }
    if !files.is_empty() {
        notes.push(format!(
            "Connection AI Memory files ready to add where missing ({})",
            files.len()
        ));
    }
    Ok(files)
}

pub(super) fn existing_catalogue(
    directory: &Directory,
) -> io::Result<Option<java_credential_restore::Connections>> {
    Ok(
        super::read_connections(directory)?.map(|contents| java_credential_restore::Connections {
            before: Some(java_credential_restore::digest(&contents)),
            contents,
            routes: Default::default(),
            memory_ids: Default::default(),
            profile_ids: Default::default(),
            groups: Default::default(),
        }),
    )
}

pub(super) fn digest(files: &Snapshot) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"kortty-java-scoped-memory-v1\0");
    for (path, bytes) in files {
        hash.update((path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes.as_slice());
    }
    hash.finalize().into()
}

pub(super) fn take(delta: &mut Snapshot, revision: [u8; 32]) -> io::Result<Snapshot> {
    let paths: Vec<_> = delta
        .iter()
        .filter(|(path, bytes)| {
            ai::memory_connection(path).is_some()
                || path.as_str() == ARCHIVE && memory::validate(path, bytes).is_err()
        })
        .map(|(path, _)| path.clone())
        .collect();
    let files = paths
        .into_iter()
        .filter_map(|path| delta.remove(&path).map(|bytes| (path, bytes)))
        .collect();
    if Snapshot::is_empty(&files) || digest(&files) != revision {
        return Err(invalid("Invalid reviewed connection AI Memory"));
    }
    Ok(files)
}

pub(super) struct Pending<'a> {
    candidate: Store,
    files: Vec<(&'a str, &'a [u8], BTreeSet<u64>)>,
}

impl<'a> Pending<'a> {
    pub(super) fn prepare(files: &'a Snapshot, connections: &[u8]) -> io::Result<Self> {
        let candidate = java_connection_candidate(connections)?;
        let files = files
            .iter()
            .map(|(path, bytes)| {
                let mut ids = BTreeSet::new();
                memory::validate_bound(path, bytes, |id| {
                    if !scope_exists(&candidate, id) {
                        return Err(invalid("A reviewed AI Memory profile is missing"));
                    }
                    ids.insert(id);
                    Ok(())
                })?;
                Ok((path.as_str(), bytes.as_slice(), ids))
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { candidate, files })
    }

    pub(super) fn publish(self, directory: &Directory, policy: &ClipboardPolicy) -> io::Result<()> {
        if !data::ai::policy_allows(ARCHIVE, policy) {
            return Ok(());
        }
        for (path, bytes, ids) in self.files {
            if !ids.iter().all(|&id| eligible(&self.candidate, id, policy)) {
                continue;
            }
            merge::publish(directory, path, bytes)?;
            merge::sync_parent(directory, path)?;
        }
        merge::sync_parent(directory, "memory/history")?;
        directory.sync()
    }
}

fn scope_exists(candidate: &Store, id: u64) -> bool {
    candidate.local(id).is_some()
        || candidate.serial(id).is_some()
        || candidate.get(id).is_some_and(|connection| {
            !connection.incognito && connection.missing_username().is_none()
        })
}

fn eligible(candidate: &Store, id: u64, policy: &ClipboardPolicy) -> bool {
    scope_exists(candidate, id)
        && (candidate.serial(id).is_none() || policy.serial_allowed)
        && candidate
            .get(id)
            .is_none_or(|connection| connection.blocked_server(policy.server_access()).is_none())
}
