//! Allowlisted AI profile records and consent-free backup projections.

use std::collections::HashSet;
use std::io;

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::ai_manager::backup as manager;
use crate::clipboard_policy::{ClipboardPolicy, UserMemoryMode};
use crate::connection_store::{self, Connection, LocalProfile, SerialProfile};
use crate::local_ai::{managed_models as gguf, mlx_models as mlx};
use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::workspace::saved_chats::backup as chats;

use super::{Snapshot, include, invalid, optional, read_file};
use crate::profile_backup::restore::with_target;

#[derive(Clone, Copy)]
enum Entry<'a> {
    Manager,
    Statistics,
    Memory,
    Chat { folder: &'a str, name: &'a str },
    Gguf(&'a str),
    Mlx(&'a str),
}

fn entry(path: &str) -> Option<Entry<'_>> {
    match path {
        manager::SETTINGS => return Some(Entry::Manager),
        manager::STATISTICS => return Some(Entry::Statistics),
        _ if manager::file_limit(path).is_some() => return Some(Entry::Memory),
        _ => {}
    }
    let (folder, name) = path.split_once('/')?;
    if chats::file_limit(folder, name).is_some() {
        return Some(Entry::Chat { folder, name });
    }
    let (id, name) = name.split_once('/')?;
    if !gguf::valid_id(id) {
        return None;
    }
    match (folder, name) {
        (gguf::BACKUP_DIRECTORY, gguf::BACKUP_MANIFEST) => Some(Entry::Gguf(id)),
        (mlx::BACKUP_DIRECTORY, mlx::BACKUP_MANIFEST) => Some(Entry::Mlx(id)),
        _ => None,
    }
}

pub(in crate::profile_backup) fn file_limit(path: &str) -> Option<usize> {
    match entry(path)? {
        Entry::Manager | Entry::Statistics | Entry::Memory => manager::file_limit(path),
        Entry::Chat { folder, name } => chats::file_limit(folder, name),
        Entry::Gguf(_) => Some(gguf::MAX_MANIFEST as usize),
        Entry::Mlx(_) => Some(mlx::MAX_MANIFEST as usize),
    }
}

pub(in crate::profile_backup) fn validate(path: &str, bytes: &[u8]) -> io::Result<()> {
    match entry(path).expect("allowlisted AI backup path") {
        Entry::Manager | Entry::Statistics | Entry::Memory => manager::validate(path, bytes),
        Entry::Chat { folder, name } => chats::validate(folder, name, bytes).map(drop),
        Entry::Gguf(id) => gguf::portable_registration(bytes, id).map(drop),
        Entry::Mlx(id) => mlx::validate_registration(bytes, id),
    }
}

pub(super) fn collect(
    directory: &Directory,
    snapshot: &mut Snapshot,
    total: &mut usize,
) -> io::Result<()> {
    let mut paths = vec![manager::SETTINGS.to_owned(), manager::STATISTICS.to_owned()];
    if let Some(memory) = optional(directory.subdirectory(manager::MEMORY, false))? {
        paths.extend(manager::MEMORY_TOP_LEVEL.map(|name| format!("{}/{name}", manager::MEMORY)));
        for folder in manager::MEMORY_DIRECTORIES {
            let Some(child) = optional(memory.subdirectory(folder, false))? else {
                continue;
            };
            paths.extend(
                child
                    .names(super::MAX_ENTRIES)?
                    .into_iter()
                    .filter_map(|name| name.into_string().ok())
                    .map(|name| format!("{}/{folder}/{name}", manager::MEMORY))
                    .filter(|path| file_limit(path).is_some()),
            );
        }
    }
    for folder in chats::DIRECTORIES {
        if let Some(child) = optional(directory.private_child(folder, false))? {
            paths.extend(
                chats::names(folder, &child)?
                    .into_iter()
                    .map(|name| format!("{folder}/{name}")),
            );
        }
    }
    for (folder, name, count) in MODEL_FOLDERS {
        if let Some(child) = optional(directory.private_child(folder, false))? {
            paths.extend(
                model_ids(&child, count)?
                    .into_iter()
                    .map(|id| format!("{folder}/{id}/{name}")),
            );
        }
    }
    manager::flush_statistics();
    for path in paths {
        let Some(bytes) = read_entry(directory, &path)? else {
            continue;
        };
        if let Some(portable) = portable(&path, bytes)? {
            include(snapshot, total, path, portable)?;
        }
    }
    Ok(())
}

const MODEL_FOLDERS: [(&str, &str, usize); 2] = [
    (
        gguf::BACKUP_DIRECTORY,
        gguf::BACKUP_MANIFEST,
        gguf::MAX_MODELS,
    ),
    (mlx::BACKUP_DIRECTORY, mlx::BACKUP_MANIFEST, mlx::MAX_MODELS),
];

fn model_ids(directory: &Directory, limit: usize) -> io::Result<Vec<String>> {
    Ok(directory
        .names(limit + 2)?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| gguf::valid_id(name))
        .collect())
}

fn read_entry(directory: &Directory, path: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let limit = file_limit(path).expect("allowlisted AI backup path");
    with_target(directory, path, false, |parent, name| {
        let Some(file) = optional(parent.open_private_file(name))? else {
            return Ok(None);
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        let bytes = read_file(&file, limit)?;
        stamp.require_unchanged(&file)?;
        Ok(Some(bytes))
    })
    .map(Option::flatten)
}

fn portable(path: &str, bytes: Zeroizing<Vec<u8>>) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    match entry(path).expect("allowlisted AI backup path") {
        Entry::Manager => manager::portable_settings(&bytes).map(Some),
        Entry::Gguf(id) => gguf::portable_registration(&bytes, id)
            .map(Zeroizing::new)
            .map(Some),
        Entry::Memory if !manager::safe_memory(path, &bytes) => Ok(None),
        Entry::Chat { folder, name } if chats::validate(folder, name, &bytes)? => Ok(None),
        _ => Ok(Some(bytes)),
    }
}

pub(in crate::profile_backup) fn prepare(snapshot: &mut Snapshot) -> io::Result<()> {
    // Validate archive entries before projecting out any local authority.
    for path in snapshot
        .keys()
        .filter(|path| entry(path).is_some())
        .cloned()
        .collect::<Vec<_>>()
    {
        let bytes = snapshot.remove(&path).expect("snapshot key");
        if let Some(bytes) = portable(&path, bytes)? {
            snapshot.insert(path, bytes);
        }
    }
    Ok(())
}

pub(super) fn validate_counts(snapshot: &Snapshot) -> io::Result<()> {
    for (folder, count) in [
        (chats::CHAT_DIRECTORY, chats::MAX_CHATS),
        (chats::SWARM_DIRECTORY, chats::MAX_SWARM_CHATS),
        (gguf::BACKUP_DIRECTORY, gguf::MAX_MODELS),
        (mlx::BACKUP_DIRECTORY, mlx::MAX_MODELS),
    ] {
        if snapshot
            .keys()
            .filter(|path| in_folder(path, folder))
            .count()
            > count
        {
            return Err(invalid("too many entries in an AI profile backup store"));
        }
    }
    Ok(())
}

fn in_folder(path: &str, folder: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(parent, _)| parent == folder)
}

pub(in crate::profile_backup) fn only_missing(path: &str) -> bool {
    matches!(
        entry(path),
        Some(Entry::Statistics | Entry::Memory | Entry::Gguf(_) | Entry::Mlx(_))
    )
}

pub(in crate::profile_backup) fn requires_private_parent(path: &str) -> bool {
    matches!(
        entry(path),
        Some(Entry::Statistics | Entry::Chat { .. } | Entry::Gguf(_) | Entry::Mlx(_))
    )
}

pub(in crate::profile_backup) fn chat_folder(path: &str) -> Option<&str> {
    match entry(path)? {
        Entry::Chat { folder, .. } => Some(folder),
        _ => None,
    }
}

pub(in crate::profile_backup) fn validate_java_chat(path: &str, bytes: &[u8]) -> io::Result<()> {
    match entry(path) {
        Some(Entry::Chat {
            folder: chats::CHAT_DIRECTORY,
            name,
        }) => chats::validate_java_chat(name, bytes),
        Some(Entry::Chat {
            folder: chats::SWARM_DIRECTORY,
            name,
        }) => chats::validate_java_swarm(name, bytes),
        _ => Err(invalid("Invalid Java AI chat path")),
    }
}

fn policy_allows(path: &str, policy: &ClipboardPolicy) -> bool {
    match entry(path) {
        Some(Entry::Manager) => {
            policy.ai_allowed
                && policy.ai_profile_creation_allowed()
                && policy.ai_profile_edit_allowed()
        }
        Some(Entry::Statistics) => {
            policy.problem.is_none()
                && policy.ai_allowed
                && policy.ai_memory.statistics != Some(false)
        }
        Some(Entry::Memory) => {
            policy.problem.is_none()
                && policy.ai_allowed
                && policy.ai_memory.feature_allowed
                && policy.ai_memory.user_memory == UserMemoryMode::Editable
        }
        Some(Entry::Gguf(_) | Entry::Mlx(_)) => policy.ai_user_model_files_allowed(),
        _ => true,
    }
}

#[derive(Default)]
pub(in crate::profile_backup) struct Restore {
    policy: Option<ClipboardPolicy>,
    connections: Option<HashSet<u64>>,
    chats: Option<ChatCapacity>,
    swarm: Option<ChatCapacity>,
}

impl Restore {
    pub(in crate::profile_backup) fn may_install(
        &mut self,
        directory: &Directory,
        snapshot: &Snapshot,
        path: &str,
    ) -> io::Result<bool> {
        let Some(kind) = entry(path) else {
            return Ok(true);
        };
        if !policy_allows(path, self.policy.get_or_insert_with(ClipboardPolicy::load)) {
            return Ok(false);
        }
        match kind {
            Entry::Statistics => {
                let current = read_entry(directory, manager::SETTINGS)?;
                Ok(manager::matching_skills(
                    current.as_ref().map(|bytes| bytes.as_slice()),
                    snapshot
                        .get(manager::SETTINGS)
                        .map(|bytes| bytes.as_slice()),
                ))
            }
            Entry::Memory => {
                let connection = manager::memory_connection(path);
                if connection.is_none() && path != "memory/archive.md" {
                    return Ok(true);
                }
                let matches = self.connections(directory, snapshot)?;
                match connection {
                    Some(id) => Ok(matches.contains(&id)),
                    None => matching_archive(matches, &snapshot[path]),
                }
            }
            Entry::Chat { folder, name } => {
                let capacity = if folder == chats::CHAT_DIRECTORY {
                    &mut self.chats
                } else {
                    &mut self.swarm
                };
                if capacity.is_none() {
                    *capacity = Some(ChatCapacity::load(directory, folder)?);
                }
                Ok(capacity.as_mut().expect("chat capacity loaded").admit(name))
            }
            Entry::Gguf(id) => model_capacity(
                directory,
                path,
                gguf::BACKUP_DIRECTORY,
                id,
                gguf::MAX_MODELS,
            ),
            Entry::Mlx(id) => {
                model_capacity(directory, path, mlx::BACKUP_DIRECTORY, id, mlx::MAX_MODELS)
            }
            _ => Ok(true),
        }
    }

    pub(in crate::profile_backup) fn rebind(
        &mut self,
        directory: &Directory,
        snapshot: &Snapshot,
        path: &str,
    ) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        if let Some(Entry::Chat {
            folder: chats::SWARM_DIRECTORY,
            name,
        }) = entry(path)
        {
            return chats::bind_swarm_routes(
                name,
                &snapshot[path],
                self.connections(directory, snapshot)?,
            );
        }
        Ok(None)
    }

    fn connections(
        &mut self,
        directory: &Directory,
        snapshot: &Snapshot,
    ) -> io::Result<&HashSet<u64>> {
        if self.connections.is_none() {
            self.connections = Some(matching_connections(directory, snapshot)?);
        }
        Ok(self
            .connections
            .as_ref()
            .expect("connection matches loaded"))
    }
}

fn model_capacity(
    root: &Directory,
    path: &str,
    folder: &str,
    id: &str,
    limit: usize,
) -> io::Result<bool> {
    let Some(models) = optional(root.private_child(folder, false))? else {
        return Ok(true);
    };
    let ids = model_ids(&models, limit)?;
    if ids.iter().any(|stored| stored == id) {
        // Never attach a restored registration to local files without a review.
        return Ok(with_target(root, path, false, |parent, _| {
            parent.names(256).map(|names| names.is_empty())
        })?
        .unwrap_or(false));
    }
    Ok(ids.len() < limit)
}

struct ChatCapacity {
    names: HashSet<String>,
    deleted: HashSet<String>,
    max_chats: usize,
    max_origins: usize,
}

impl ChatCapacity {
    fn load(root: &Directory, folder: &str) -> io::Result<Self> {
        let swarm = folder == chats::SWARM_DIRECTORY;
        let mut names = HashSet::new();
        let mut deleted = HashSet::new();
        if let Some(directory) = optional(root.private_child(folder, false))? {
            for name in chats::names(folder, &directory)? {
                if swarm && swarm_deleted(&directory, &name)? {
                    deleted.insert(name.clone());
                }
                names.insert(name);
            }
        }
        Ok(Self {
            names,
            deleted,
            max_chats: if swarm {
                chats::MAX_SWARM_CHATS
            } else {
                chats::MAX_CHATS
            },
            max_origins: if swarm {
                chats::MAX_SWARM_ORIGINS
            } else {
                chats::MAX_CHATS
            },
        })
    }

    fn admit(&mut self, name: &str) -> bool {
        if self.names.contains(name) {
            // A local tombstone must keep a scheduler origin reserved.
            return !self.deleted.contains(name);
        }
        if self.names.len() >= self.max_origins
            || self.names.len() - self.deleted.len() >= self.max_chats
        {
            return false;
        }
        self.names.insert(name.to_owned());
        true
    }
}

fn swarm_deleted(directory: &Directory, name: &str) -> io::Result<bool> {
    let Some(file) = optional(directory.open_private_file(name))? else {
        return Ok(false);
    };
    // A tombstone contains only its version and the short origin identifier.
    const TOMBSTONE_LIMIT: usize = 1024;
    if file.metadata()?.len() > TOMBSTONE_LIMIT as u64 {
        return Ok(false);
    }
    let bytes = read_file(&file, TOMBSTONE_LIMIT)?;
    // Corrupt records still occupy an active slot.
    Ok(chats::validate(chats::SWARM_DIRECTORY, name, &bytes).unwrap_or(false))
}

#[derive(Deserialize)]
struct Connections {
    connections: Vec<Connection>,
    #[serde(default)]
    local_profiles: Vec<LocalProfile>,
    #[serde(default)]
    serial_profiles: Vec<SerialProfile>,
}

impl Connections {
    fn matching(&self, other: &Self) -> HashSet<u64> {
        self.connections
            .iter()
            .filter(|row| other.connections.contains(row))
            .map(|row| row.id)
            .chain(
                self.local_profiles
                    .iter()
                    .filter(|row| other.local_profiles.contains(row))
                    .map(|row| row.id),
            )
            .chain(
                self.serial_profiles
                    .iter()
                    .filter(|row| other.serial_profiles.contains(row))
                    .map(|row| row.id),
            )
            .collect()
    }
}

fn matching_connections(root: &Directory, snapshot: &Snapshot) -> io::Result<HashSet<u64>> {
    let Some(incoming) = snapshot.get("connections.json") else {
        return Ok(HashSet::new());
    };
    let Some(file) = optional(root.open_file("connections.json"))? else {
        return Ok(HashSet::new());
    };
    let current = read_file(&file, super::file_limit("connections.json"))?;
    let (Ok(incoming), Ok(current)) = (
        serde_json::from_slice::<Connections>(incoming),
        connection_store::portable_backup(&current).and_then(|bytes| {
            serde_json::from_slice::<Connections>(&bytes).map_err(io::Error::other)
        }),
    ) else {
        return Ok(HashSet::new());
    };
    Ok(incoming.matching(&current))
}

fn matching_archive(matches: &HashSet<u64>, bytes: &[u8]) -> io::Result<bool> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let text = std::str::from_utf8(bytes).map_err(invalid)?;
    for marker in text.lines().filter_map(|line| {
        line.strip_prefix("<!-- kortty:archived scope=connection:")?
            .strip_suffix(" -->")
    }) {
        let id = URL_SAFE_NO_PAD
            .decode(marker)
            .ok()
            .and_then(|bytes| std::str::from_utf8(&bytes).ok()?.parse().ok());
        if id.is_some_and(|id| !matches.contains(&id)) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_backup_restore_applies_independent_policy_and_accepts_only_known_paths() {
        let mut policy = ClipboardPolicy::default();
        assert!(policy_allows("memory/MEMORY.md", &policy));
        assert!(policy_allows(manager::STATISTICS, &policy));
        policy.ai_memory.user_memory = UserMemoryMode::ReadOnly;
        policy.ai_memory.statistics = Some(false);
        policy.ai_profile_creation_allowed = false;
        assert!(!policy_allows("memory/MEMORY.md", &policy));
        assert!(!policy_allows(manager::STATISTICS, &policy));
        assert!(!policy_allows(manager::SETTINGS, &policy));
        for path in [
            "memory/external.json",
            "memory/connections/../MEMORY.md",
            "memory/connections/0001.md",
            "memory/history/connection-1-20261112-093535-043.md/extra",
            "stats/.skill-usage.lock",
            "llama-models/.staging/model-v1.json",
            "mlx-models/11111111111111111111111111111111/model.safetensors",
            "swarm-conversations/active-state.json",
        ] {
            assert!(file_limit(path).is_none(), "{path}");
        }
    }
}
