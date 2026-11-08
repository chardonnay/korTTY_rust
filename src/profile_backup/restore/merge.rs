//! No-replace startup replay. Preserve local IDs and their credential bindings.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{self, Read};

use kortty_jobs::SchedulerStore;
use serde::{Deserialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::{
    Directory, JOBS, PINS, RECORDINGS, Snapshot, missing, path_error, read_bounded, with_target,
};
use crate::connection_store::{self, Auth, Connection};
use crate::scheduler_window::{ConnectionRef, RemoteAction, SshCommand};
use crate::snippets::SnippetId;
use crate::ssh_key_catalog::KeyRecord;

const VAULT: &str = "vault/vault.bin";
const KEY_CATALOG: &str = "ssh-keys.json";
const KEY_DIR: &str = "ssh-keys/";
const SNIPPETS: &str = "snippets.json";
const CONNECTIONS: &str = "connections.json";

const LINKED: &[&str] = &[VAULT, KEY_CATALOG, SNIPPETS, CONNECTIONS, JOBS];
const MAX_METADATA: usize = 1024 * 1024;

pub(super) fn apply(
    directory: &Directory,
    snapshot: &Snapshot,
    protect_pins: &impl Fn() -> bool,
) -> io::Result<()> {
    let independent = snapshot
        .iter()
        .filter(|(path, _)| !LINKED.contains(&path.as_str()) && !path.starts_with(KEY_DIR));
    let mut new_files = Vec::new();
    let mut nested = BTreeMap::new();
    for (path, bytes) in independent {
        if path == PINS && protect_pins() {
            continue;
        }
        if let Some((parent, _)) = path.split_once('/') {
            nested.entry(parent).or_insert(path.as_str());
        }
        if !with_target(directory, path, false, Directory::has_entry)
            .map_err(|error| path_error(path, error))?
            .unwrap_or(false)
        {
            new_files.push((path, bytes));
        }
    }

    if LINKED.iter().any(|path| snapshot.contains_key(*path)) {
        LinkedMerge {
            directory,
            snapshot,
        }
        .apply()?;
    }

    for (path, bytes) in new_files {
        if path != PINS || !protect_pins() {
            publish(directory, path, bytes)?;
        }
    }
    // A previous replay may have published a file before its directory sync
    // failed. Retry the sync even when there was no new rename this time.
    for path in nested.into_values() {
        sync_parent(directory, path)?;
    }
    directory.sync()
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Existing {
    Missing,
    Same,
    Different,
}

fn existing(directory: &Directory, path: &str, source: Option<&[u8]>) -> io::Result<Existing> {
    let found = with_target(directory, path, false, |parent, name| {
        let Some(bytes) = source else {
            return parent.has_entry(name).map(|exists| {
                if exists {
                    Existing::Different
                } else {
                    Existing::Missing
                }
            });
        };
        let opened = if path == KEY_CATALOG || path.starts_with(KEY_DIR) {
            parent.open_private_file(name)
        } else {
            parent.open_file(name)
        };
        match missing(opened) {
            Ok(None) => Ok(Existing::Missing),
            Ok(Some(file)) if path == CONNECTIONS => {
                // Local tunnel approvals do not travel in a backup and must
                // survive merge. Compare the same projection used at export.
                let same = read_bounded(file, MAX_METADATA).is_ok_and(|local| {
                    local.as_slice() == bytes
                        || connection_store::portable_backup(&local)
                            .is_ok_and(|portable| portable == bytes)
                });
                Ok(if same {
                    Existing::Same
                } else {
                    Existing::Different
                })
            }
            Ok(Some(file)) => same_bytes(file, bytes).map(|same| {
                if same {
                    Existing::Same
                } else {
                    Existing::Different
                }
            }),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::PermissionDenied
                ) =>
            {
                Ok(Existing::Different)
            }
            Err(error) => Err(error),
        }
    })
    .map_err(|error| path_error(path, error))?;
    Ok(found.unwrap_or(Existing::Missing))
}

fn same_bytes(mut file: std::fs::File, bytes: &[u8]) -> io::Result<bool> {
    if file.metadata()?.len() != bytes.len() as u64 {
        return Ok(false);
    }
    let mut buffer = Zeroizing::new([0; 8192]);
    for chunk in bytes.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])?;
        if chunk != &buffer[..chunk.len()] {
            return Ok(false);
        }
    }
    Ok(file.read(&mut buffer[..1])? == 0)
}

fn publish(directory: &Directory, path: &str, bytes: &[u8]) -> io::Result<bool> {
    let installed = with_target(directory, path, true, |parent, name| {
        match parent.publish_copy(name, bytes) {
            Ok(()) => parent.sync().map(|()| true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    })
    .map_err(|error| path_error(path, error))?;
    Ok(installed.unwrap_or(false))
}

fn sync_parent(directory: &Directory, path: &str) -> io::Result<()> {
    with_target(directory, path, false, |parent, _| parent.sync())
        .map_err(|error| path_error(path, error))?;
    Ok(())
}

struct LinkedMerge<'a> {
    directory: &'a Directory,
    snapshot: &'a Snapshot,
}

impl LinkedMerge<'_> {
    fn apply(&self) -> io::Result<()> {
        // Check all target parents and known collisions before writing. An
        // exact match represents a usable prerequisite, including after retry.
        let vault = self.state(VAULT)?;
        let catalog = self.state(KEY_CATALOG)?;
        let snippets = self.state(SNIPPETS)?;
        let connections = self.state(CONNECTIONS)?;
        let jobs = self.state(JOBS)?;
        let keys_clear = self.key_paths_are_compatible()?;

        let keys: Keys = self.source(KEY_CATALOG)?;
        let profiles: Connections = self.source(CONNECTIONS)?;
        let source_snippets: Snippets = if self.snapshot.contains_key(JOBS) {
            self.source(SNIPPETS)?
        } else {
            Snippets::default()
        };

        // Adding a previously missing store can reactivate references from a
        // different local profile. Read those consumers before installing any
        // prerequisite; unreadable local metadata is conservatively retained.
        let local_keys = self.local::<Keys>(KEY_CATALOG, catalog, MAX_METADATA);
        let local_profiles = self.local::<Connections>(CONNECTIONS, connections, MAX_METADATA);
        let local_jobs = self.local_job_links(jobs);
        let vault_free = local_keys.is_some_and(|keys| !keys.uses_vault())
            && local_profiles
                .as_ref()
                .is_some_and(|profiles| !profiles.uses_vault())
            && !local_jobs.vault;
        let keys_free = local_profiles.is_some_and(|profiles| !profiles.uses_catalog());
        let can_add_jobs = jobs != Existing::Missing || !self.directory.has_entry(RECORDINGS)?;

        // Install dependencies in order. Each step is no-replace, and a late
        // collision only suppresses additions that require the collided file.
        let vault_ready = self.install(VAULT, vault, vault_free)?;
        if vault_ready {
            sync_parent(self.directory, VAULT)?;
        }
        let keys_ready = keys_clear
            && (vault_ready || !keys.uses_vault())
            && self.install_keys(catalog, keys_free)?;
        let snippets_ready = self.install(SNIPPETS, snippets, !local_jobs.snippets)?;
        let connections_ready = profiles.links_exist(vault_ready, keys_ready, &keys)
            && self.install(CONNECTIONS, connections, !local_jobs.connections)?;

        if can_add_jobs && let Some(bytes) = self.snapshot.get(JOBS) {
            let routes: Vec<_> = profiles
                .connections
                .iter()
                .map(ConnectionRef::from)
                .collect();
            if SchedulerStore::validate_backup(bytes, |action: SshCommand| {
                action
                    .targets()
                    .all(|route| connections_ready && routes.contains(route))
                    && action.snippet.as_ref().is_none_or(|link| {
                        snippets_ready
                            && source_snippets
                                .snippets
                                .iter()
                                .any(|snippet| snippet.id == link.id)
                    })
                    && (vault_ready || !JobLinks::for_action(&action).vault)
            })
            .is_ok()
            {
                self.install(JOBS, jobs, true)?;
            }
        }
        Ok(())
    }

    fn state(&self, path: &str) -> io::Result<Existing> {
        existing(
            self.directory,
            path,
            self.snapshot.get(path).map(|bytes| bytes.as_slice()),
        )
    }

    fn key_paths(&self) -> impl Iterator<Item = &str> {
        self.snapshot
            .keys()
            .map(String::as_str)
            .filter(|path| path.starts_with(KEY_DIR))
    }

    fn key_paths_are_compatible(&self) -> io::Result<bool> {
        for path in self.key_paths() {
            if self.state(path)? == Existing::Different {
                return Ok(false);
            }
            if !path.ends_with(".pub") {
                let companion = format!("{path}.pub");
                if !self.snapshot.contains_key(&companion)
                    && self.state(&companion)? != Existing::Missing
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn install_keys(&self, catalog: Existing, can_add: bool) -> io::Result<bool> {
        if catalog == Existing::Different
            || (catalog == Existing::Missing && !can_add)
            || !self.snapshot.contains_key(KEY_CATALOG)
        {
            return Ok(false);
        }
        // The catalog only becomes visible after all its key material.
        for path in self.key_paths() {
            if !self.install(path, self.state(path)?, true)? {
                return Ok(false);
            }
        }
        if let Some(key) = self.key_paths().next() {
            sync_parent(self.directory, key)?;
        }
        self.install(KEY_CATALOG, catalog, can_add)
    }

    fn install(&self, path: &str, state: Existing, can_add: bool) -> io::Result<bool> {
        let Some(bytes) = self.snapshot.get(path) else {
            return Ok(false);
        };
        match state {
            Existing::Same => Ok(true),
            Existing::Different => Ok(false),
            Existing::Missing if can_add => {
                Ok(publish(self.directory, path, bytes)? || self.state(path)? == Existing::Same)
            }
            Existing::Missing => Ok(false),
        }
    }

    fn source<T: DeserializeOwned + Default>(&self, path: &str) -> io::Result<T> {
        self.snapshot.get(path).map_or_else(
            || Ok(T::default()),
            |bytes| serde_json::from_slice(bytes).map_err(io::Error::other),
        )
    }

    fn local<T: DeserializeOwned + Default>(
        &self,
        path: &str,
        state: Existing,
        limit: usize,
    ) -> Option<T> {
        if state != Existing::Different {
            return Some(T::default());
        }
        let bytes = self.local_bytes(path, limit)?;
        serde_json::from_slice(&bytes).ok()
    }

    fn local_bytes(&self, path: &str, limit: usize) -> Option<Zeroizing<Vec<u8>>> {
        let file = if path == KEY_CATALOG {
            self.directory.open_private_file(path)
        } else {
            self.directory.open_file(path)
        }
        .ok()?;
        read_bounded(file, limit).ok()
    }

    fn local_job_links(&self, state: Existing) -> JobLinks {
        if state != Existing::Different {
            return JobLinks::default();
        }
        let links = self
            .local_bytes(JOBS, kortty_jobs::MAX_STORE_BYTES as usize)
            .and_then(|bytes| SchedulerStore::portable_backup(&bytes).ok())
            .map(Zeroizing::new)
            .and_then(|bytes| JobLinks::read(&bytes));
        links.unwrap_or(JobLinks {
            vault: true,
            connections: true,
            snippets: true,
        })
    }
}

#[derive(Default, Deserialize)]
struct Connections {
    connections: Vec<Connection>,
}

impl Connections {
    fn auth_methods(&self) -> impl Iterator<Item = &Auth> {
        self.connections.iter().flat_map(|connection| {
            std::iter::once(&connection.login)
                .chain(connection.jump.iter())
                .map(|login| &login.auth)
        })
    }

    fn uses_vault(&self) -> bool {
        self.auth_methods().any(auth_uses_vault)
    }

    fn uses_catalog(&self) -> bool {
        self.auth_methods()
            .any(|auth| auth.registered_key().is_some())
    }

    fn links_exist(&self, vault_ready: bool, keys_ready: bool, keys: &Keys) -> bool {
        self.auth_methods().all(|auth| {
            let key_available = auth
                .registered_key()
                .is_none_or(|id| keys_ready && keys.records.iter().any(|key| key.id == id));
            key_available && (vault_ready || !auth_uses_vault(auth))
        })
    }
}

fn auth_uses_vault(auth: &Auth) -> bool {
    auth.has_vault_reference()
}

#[derive(Default, Deserialize)]
struct Keys {
    records: Vec<KeyRecord>,
}

impl Keys {
    fn uses_vault(&self) -> bool {
        self.records.iter().any(|key| key.passphrase.is_some())
    }
}

#[derive(Default, Deserialize)]
struct Snippets {
    snippets: Vec<SnippetIdentity>,
}

#[derive(Deserialize)]
struct SnippetIdentity {
    id: SnippetId,
}

#[derive(Clone, Copy, Default)]
struct JobLinks {
    vault: bool,
    connections: bool,
    snippets: bool,
}

impl JobLinks {
    fn for_action(action: &SshCommand) -> Self {
        Self {
            vault: matches!(&action.remote, Some(RemoteAction::Archive(archive)) if archive.password_credential.is_some()),
            connections: action.targets().next().is_some(),
            snippets: action.snippet.is_some(),
        }
    }

    fn read(bytes: &[u8]) -> Option<Self> {
        let links = Cell::new(Self::default());
        SchedulerStore::validate_backup(bytes, |action: SshCommand| {
            let mut known = links.get();
            let current = Self::for_action(&action);
            known.vault |= current.vault;
            known.connections |= current.connections;
            known.snippets |= current.snippets;
            links.set(known);
            true
        })
        .ok()?;
        Some(links.get())
    }
}
