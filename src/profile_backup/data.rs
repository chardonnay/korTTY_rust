//! Portable profile data. Read only named stores, never a recursive profile walk.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use kortty_jobs::SchedulerStore;
use kortty_ssh::TrustStore;
use kortty_vault::Vault;
use zeroize::Zeroizing;

use crate::private_files::Directory;
use crate::session_journal::markers;
use crate::workspace::backup_data;
use crate::{connection_store, profile, scheduler_window::SshCommand, snippets, ssh_key_catalog};

use super::Snapshot;

#[cfg(test)]
mod tests;

pub(super) const STATIC_PATHS: &[&str] = &[
    "connections.json",
    "environments.json",
    "jobs.json",
    markers::FILE,
    "openpgp-recipients.json",
    "snippets.json",
    "ssh-keys.json",
    "ssh/known_hosts",
    "toolbar.json",
    "vault/vault.bin",
    "window-settings.json",
];

const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_PROJECTS: usize = 256;
const PROJECT_DIR: &str = "projects";
const KEY_CATALOG: &str = "ssh-keys.json";
const VAULT: &str = "vault/vault.bin";

pub(super) fn collect(root: &Path) -> io::Result<Snapshot> {
    let mut snapshot = Snapshot::new();
    let mut total = 0;
    let Some(directory) = optional(Directory::open(root, false))? else {
        return Ok(snapshot);
    };
    for &path in STATIC_PATHS {
        if path == KEY_CATALOG {
            continue;
        }
        let Some(file) = optional(open_path(&directory, path))? else {
            continue;
        };
        let bytes = read_file(file, file_limit(path))?;
        let bytes = match path {
            "connections.json" => Zeroizing::new(connection_store::portable_backup(&bytes)?),
            "jobs.json" => {
                Zeroizing::new(SchedulerStore::portable_backup(&bytes).map_err(invalid)?)
            }
            _ => bytes,
        };
        include(&mut snapshot, &mut total, path.into(), bytes)?;
    }
    if let Some(projects) = optional(directory.subdirectory(PROJECT_DIR, false))? {
        for name in project_names(&projects)? {
            let path = format!("{PROJECT_DIR}/{name}");
            let bytes = read_file(projects.open_file(&name)?, file_limit(&path))?;
            include(&mut snapshot, &mut total, path, bytes)?;
        }
    }
    ssh_key_catalog::collect_backup(root, |path, bytes| {
        include(&mut snapshot, &mut total, path, bytes)
    })?;
    if snapshot.contains_key(KEY_CATALOG) {
        // Catalog saves can add a vault passphrase before committing metadata.
        // A changed vault must not be paired with the just-captured catalog.
        let vault = optional(open_path(&directory, VAULT))?
            .map(|file| read_file(file, file_limit(VAULT)))
            .transpose()?;
        if vault.as_ref() != snapshot.get(VAULT) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the vault changed during the profile backup; try again",
            ));
        }
    }
    validate(&snapshot)?;
    Ok(snapshot)
}

pub(super) fn existing_project_paths(root: &Path) -> io::Result<Vec<String>> {
    let Some(root) = optional(Directory::open(root, false))? else {
        return Ok(Vec::new());
    };
    let Some(projects) = optional(root.subdirectory(PROJECT_DIR, false))? else {
        return Ok(Vec::new());
    };
    project_names(&projects)?
        .into_iter()
        .map(|name| {
            projects.open_file(&name)?;
            Ok(format!("{PROJECT_DIR}/{name}"))
        })
        .collect()
}

fn project_names(projects: &Directory) -> io::Result<Vec<String>> {
    let mut names: Vec<_> = projects
        .names(MAX_ENTRIES)?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| profile::projects::is_backup_name(name))
        .collect();
    if names.len() > MAX_PROJECTS {
        return Err(invalid("too many projects for a profile backup"));
    }
    names.sort_unstable();
    Ok(names)
}

pub(super) fn validate(snapshot: &Snapshot) -> io::Result<()> {
    if snapshot.len() > MAX_ENTRIES {
        return Err(invalid("too many files in a profile backup"));
    }
    let mut total = 0;
    let mut project_count = 0;
    let managed = snapshot.get(KEY_CATALOG).map_or_else(
        || Ok(Vec::new()),
        |bytes| ssh_key_catalog::backup_managed_names(bytes),
    )?;
    let mut expected_keys: HashSet<_> = managed.iter().map(String::as_str).collect();
    let allowed_keys = expected_keys.clone();
    for (path, bytes) in snapshot {
        if !allowed_path(path) || bytes.len() > file_limit(path) {
            return Err(invalid("unsupported or oversized profile backup entry"));
        }
        total += bytes.len();
        if total > MAX_TOTAL_BYTES {
            return Err(invalid("profile backup data exceed 128 MiB"));
        }
        match path.as_str() {
            "connections.json" => connection_store::validate_backup(bytes)?,
            "jobs.json" => SchedulerStore::validate_backup(bytes, |action: SshCommand| {
                action.validated().is_ok()
            })
            .map_err(invalid)?,
            markers::FILE => markers::validate_backup(bytes)?,
            "environments.json" | "toolbar.json" | "window-settings.json" => {
                backup_data::validate(path, bytes)?;
            }
            "openpgp-recipients.json" => snippets::recipients::validate_backup(bytes)?,
            "snippets.json" => snippets::validate_backup(bytes)?,
            "ssh/known_hosts" => TrustStore::validate_backup(bytes)?,
            "vault/vault.bin" => Vault::validate_backup_envelope(bytes).map_err(invalid)?,
            KEY_CATALOG => {}
            _ => {
                if let Some(name) = path.strip_prefix("ssh-keys/") {
                    if let Some(primary) = name.strip_suffix(".pub") {
                        if !allowed_keys.contains(primary) {
                            return Err(invalid("a public SSH key has no catalog entry"));
                        }
                    } else {
                        if bytes.is_empty() {
                            return Err(invalid("a managed SSH key backup entry is empty"));
                        }
                        if !expected_keys.remove(name) {
                            return Err(invalid("a managed SSH key has no catalog entry"));
                        }
                    }
                } else {
                    project_count += 1;
                    if project_count > MAX_PROJECTS {
                        return Err(invalid("too many projects in a profile backup"));
                    }
                    profile::projects::validate_backup(bytes)?;
                }
            }
        }
    }
    if !expected_keys.is_empty() {
        return Err(invalid("a catalog entry is missing its managed SSH key"));
    }
    Ok(())
}

pub(super) fn allowed_path(path: &str) -> bool {
    STATIC_PATHS.contains(&path)
        || path
            .strip_prefix("projects/")
            .is_some_and(profile::projects::is_backup_name)
        || path
            .strip_prefix("ssh-keys/")
            .is_some_and(ssh_key_catalog::is_backup_managed_name)
}

fn file_limit(path: &str) -> usize {
    match path {
        "snippets.json" => 64 * 1024 * 1024,
        "jobs.json" => 16 * 1024 * 1024,
        markers::FILE => markers::MAX_FILE_BYTES,
        "openpgp-recipients.json" => 8 * 1024 * 1024,
        "vault/vault.bin" => 4 * 1024 * 1024,
        _ if path.starts_with("projects/") => 256 * 1024,
        _ => 1024 * 1024,
    }
}

fn open_path(directory: &Directory, path: &str) -> io::Result<File> {
    match path.split_once('/') {
        Some((parent, name)) => directory.subdirectory(parent, false)?.open_file(name),
        None => directory.open_file(path),
    }
}

fn read_file(file: File, maximum: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let expected = file.metadata()?.len();
    if expected > maximum as u64 {
        return Err(invalid("profile backup source file exceeds its size limit"));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    bytes
        .try_reserve_exact(expected as usize + 1)
        .map_err(invalid)?;
    file.take(expected + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected {
        return Err(invalid(
            "profile backup source file changed while being read",
        ));
    }
    Ok(bytes)
}

fn include(
    snapshot: &mut Snapshot,
    total: &mut usize,
    path: String,
    bytes: Zeroizing<Vec<u8>>,
) -> io::Result<()> {
    *total += bytes.len();
    if *total > MAX_TOTAL_BYTES || snapshot.len() >= MAX_ENTRIES {
        return Err(invalid(
            "profile backup exceeds its size or file count limit",
        ));
    }
    if snapshot.insert(path, bytes).is_some() {
        return Err(invalid("duplicate profile backup entry"));
    }
    Ok(())
}

fn optional<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
