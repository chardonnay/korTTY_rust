//! A bounded semantic index over approved text and PDF sources.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::knowledge_pdf as pdf;
use crate::local_ai::{Client, Profile};
use crate::private_files::{Directory, FileStamp, nonce};

use super::ProfileRef;

pub(super) mod folder;
mod hnsw;
pub(super) mod qdrant;
pub(super) mod remote_index;
pub(in crate::ai_manager) mod retrieval;
mod snapshot;
pub(super) mod source;
mod source_mask;
pub(crate) mod sync;
mod text_stream;
pub(super) use snapshot::Cache;
use source::FileSizeLimit;
pub(super) use source_mask::SourceMask;
#[cfg(test)]
mod scale;

pub(super) const MAX_STORES: usize = 32;
const MAX_SOURCES: usize = 5_000;
const LEGACY_MAX_SOURCES: usize = 32;
const CHUNK_BYTES: usize = 4096;
const MAX_CHUNKS: usize = 8_192;
const MAX_INDEX_PASSAGES: usize = 16_384;
const MAX_DIMENSIONS: usize = 16_384;
const BATCH_SIZE: usize = 16;
const SEARCH_HITS: usize = 5;
const EXACT_SEARCH_LIMIT: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Store {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) embedding: Option<Embedding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) qdrant: Option<qdrant::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) coding_profile: Option<ProfileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) text_profile: Option<ProfileRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) autonomous_enabled: bool,
    pub(super) name: String,
    pub(super) sources: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) folders: Vec<folder::Folder>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    disabled_files: BTreeSet<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    file_size_limits: BTreeMap<PathBuf, FileSizeLimit>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    disabled_folders: BTreeSet<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    automatic_files: BTreeSet<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    automatic_pdf_files: BTreeSet<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    automatic_file_size_limits: BTreeMap<PathBuf, FileSizeLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) automatic_embedding: Option<Embedding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) automatic_policy_model: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) automatic_qdrant: Option<qdrant::Settings>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) automatic_qdrant_expanded: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum SourceKind {
    File,
    Folder,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Embedding {
    pub(super) endpoint: String,
    pub(super) model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) managed_model: Option<String>,
}

impl Store {
    pub(super) fn prepare_backup(&mut self) {
        self.automatic_files.clear();
        self.automatic_pdf_files.clear();
        self.automatic_file_size_limits.clear();
        self.automatic_embedding = None;
        self.automatic_policy_model = None;
        self.automatic_qdrant = None;
        self.automatic_qdrant_expanded = false;
        self.file_size_limits
            .retain(|_, limit| !limit.is_reviewed());
        for folder in &mut self.folders {
            folder.prepare_backup();
        }
        if let Some(qdrant) = &mut self.qdrant {
            qdrant.credential_env.clear();
            qdrant.vault_key = None;
        }
    }

    pub(super) fn new(name: &str) -> io::Result<Self> {
        Ok(Self {
            id: Some(nonce()?),
            embedding: None,
            qdrant: None,
            coding_profile: None,
            text_profile: None,
            autonomous_enabled: false,
            name: name.into(),
            sources: Vec::new(),
            folders: Vec::new(),
            disabled_files: BTreeSet::new(),
            file_size_limits: BTreeMap::new(),
            disabled_folders: BTreeSet::new(),
            automatic_files: BTreeSet::new(),
            automatic_pdf_files: BTreeSet::new(),
            automatic_file_size_limits: BTreeMap::new(),
            automatic_embedding: None,
            automatic_policy_model: None,
            automatic_qdrant: None,
            automatic_qdrant_expanded: false,
        })
    }

    pub(super) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(super) fn add_source(&mut self, path: PathBuf) -> Result<(), String> {
        source_name(&path).map_err(|error| error.to_string())?;
        if self.source_count() >= MAX_SOURCES {
            return Err(format!(
                "A knowledge store supports up to {MAX_SOURCES} files"
            ));
        }
        if self.sources.contains(&path)
            || self
                .folders
                .iter()
                .flat_map(folder::Folder::sources)
                .any(|(source, _)| folder::same_path(&source, &path))
        {
            return Err("This source is already in the store".into());
        }
        self.sources.push(path);
        Ok(())
    }

    pub(super) fn source_count(&self) -> usize {
        self.sources.len() + self.folders.iter().map(folder::Folder::len).sum::<usize>()
    }

    pub(super) fn source_enabled(&self, kind: SourceKind, path: &Path) -> bool {
        !match kind {
            SourceKind::File => &self.disabled_files,
            SourceKind::Folder => &self.disabled_folders,
        }
        .contains(path)
    }

    pub(super) fn has_enabled_sources(&self) -> bool {
        self.sources
            .iter()
            .any(|path| self.source_enabled(SourceKind::File, path))
            || self.folders.iter().any(|folder| {
                folder.len() != 0 && self.source_enabled(SourceKind::Folder, &folder.root)
            })
    }

    pub(super) fn has_automatic_sources(&self) -> bool {
        self.sources.iter().any(|path| {
            self.file_sync(path) == source::SyncMode::Automatic
                && self.source_enabled(SourceKind::File, path)
        }) || self.folders.iter().any(|folder| {
            folder.options.sync == source::SyncMode::Automatic
                && self.source_enabled(SourceKind::Folder, &folder.root)
        })
    }

    pub(super) fn file_sync(&self, path: &Path) -> source::SyncMode {
        let approved = if pdf::is_pdf(path) {
            &self.automatic_pdf_files
        } else {
            &self.automatic_files
        };
        if approved.contains(path)
            && self.file_size_limits.get(path) == self.automatic_file_size_limits.get(path)
        {
            source::SyncMode::Automatic
        } else {
            source::SyncMode::Manual
        }
    }

    pub(super) fn set_file_sync(&mut self, path: PathBuf, sync: source::SyncMode) -> bool {
        if !self.sources.contains(&path) {
            return false;
        }
        let approved = if pdf::is_pdf(&path) {
            self.automatic_files.remove(&path);
            &mut self.automatic_pdf_files
        } else {
            &mut self.automatic_files
        };
        let changed = match sync {
            source::SyncMode::Manual => approved.remove(&path),
            source::SyncMode::Automatic => approved.insert(path.clone()),
        };
        let limit = (sync == source::SyncMode::Automatic)
            .then(|| self.file_limit(&path))
            .flatten();
        let previous = if let Some(limit) = limit {
            self.automatic_file_size_limits.insert(path, limit)
        } else {
            self.automatic_file_size_limits.remove(&path)
        };
        changed || previous != limit
    }

    pub(super) fn file_size_limit(&self, path: &Path) -> Option<u64> {
        self.file_limit(path).map(FileSizeLimit::bytes)
    }

    pub(super) fn effective_file_size_limit(&self, path: &Path) -> u64 {
        source::effective_file_size(path, self.file_limit(path))
    }

    fn file_limit(&self, path: &Path) -> Option<FileSizeLimit> {
        self.file_size_limits.get(path).copied()
    }

    pub(super) fn set_file_size_limit(
        &mut self,
        path: PathBuf,
        limit: Option<u64>,
    ) -> Result<bool, String> {
        source::validate_file_size(limit)?;
        if !self.sources.contains(&path) {
            return Err("The selected file is no longer in the store.".into());
        }
        let limit = limit.map(|bytes| FileSizeLimit::reviewed_for_text(bytes, !pdf::is_pdf(&path)));
        if let Some(limit) = limit {
            validate_source_limit(&path, limit)?;
        }
        if self.file_limit(&path) == limit {
            return Ok(false);
        }
        // Changing the boundary requires the file's automatic sync approval again.
        self.set_file_sync(path.clone(), source::SyncMode::Manual);
        if let Some(limit) = limit {
            self.file_size_limits.insert(path, limit);
        } else {
            self.file_size_limits.remove(&path);
        }
        Ok(true)
    }

    pub(super) fn set_source_enabled(
        &mut self,
        kind: SourceKind,
        path: PathBuf,
        enabled: bool,
    ) -> bool {
        let disabled = match kind {
            SourceKind::File if self.sources.contains(&path) => &mut self.disabled_files,
            SourceKind::Folder if self.folders.iter().any(|folder| folder.root == path) => {
                &mut self.disabled_folders
            }
            _ => return false,
        };
        if enabled {
            disabled.remove(&path)
        } else {
            disabled.insert(path)
        }
    }

    pub(super) fn remove_file(&mut self, index: usize) {
        let path = self.sources.remove(index);
        self.disabled_files.remove(&path);
        self.file_size_limits.remove(&path);
        self.automatic_files.remove(&path);
        self.automatic_pdf_files.remove(&path);
        self.automatic_file_size_limits.remove(&path);
    }

    pub(super) fn remove_folder(&mut self, index: usize) {
        self.disabled_folders
            .remove(&self.folders.remove(index).root);
    }

    #[cfg(test)]
    pub(super) fn folder_review(
        &self,
        root: PathBuf,
    ) -> Result<(folder::ReviewReply, folder::Cancel), String> {
        let options = self
            .folders
            .iter()
            .find(|folder| folder::same_path(&folder.root, &root))
            .map(|folder| folder.options.clone())
            .unwrap_or_default();
        self.folder_review_options(root, options)
    }

    pub(super) fn folder_review_options(
        &self,
        root: PathBuf,
        options: source::Options,
    ) -> Result<(folder::ReviewReply, folder::Cancel), String> {
        options.validate()?;
        let existing = self.folder_index(&root)?;
        let previous = existing.map(|index| self.folders[index].clone());
        if previous
            .as_ref()
            .is_some_and(|folder| !self.source_enabled(SourceKind::Folder, &folder.root))
        {
            return Err("Enable this folder before reviewing an update.".into());
        }
        let current = previous.as_ref().map_or(0, folder::Folder::len);
        folder::start(
            root,
            previous,
            MAX_SOURCES - self.source_count() + current,
            self.sources.clone(),
            options,
        )
    }

    pub(super) fn approve_folder(&mut self, review: &folder::Review) -> Result<(), String> {
        let position = self.folder_index(&review.folder.root)?;
        if review.existing != position.is_some() || (position.is_none() && review.folder.len() == 0)
        {
            return Err("The selected folder or store changed. Review the folder again.".into());
        }
        let mut candidate = self.clone();
        match position {
            Some(position) => {
                let old = &candidate.folders[position].root;
                if candidate.disabled_folders.remove(old) {
                    candidate
                        .disabled_folders
                        .insert(review.folder.root.clone());
                }
                candidate.folders[position] = review.folder.clone();
            }
            None => candidate.folders.push(review.folder.clone()),
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    fn folder_index(&self, root: &Path) -> Result<Option<usize>, String> {
        folder::validate_root(root)?;
        let position = self
            .folders
            .iter()
            .position(|folder| folder::same_path(&folder.root, root));
        if self.folders.iter().enumerate().any(|(index, folder)| {
            Some(index) != position
                && (folder::within(root, &folder.root) || folder::within(&folder.root, root))
        }) {
            return Err("This folder overlaps another reviewed folder in the store.".into());
        }
        if position.is_none() && self.folders.len() == folder::MAX_FOLDERS {
            return Err(format!(
                "A knowledge store supports up to {} folders.",
                folder::MAX_FOLDERS
            ));
        }
        Ok(position)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        for settings in [&self.qdrant, &self.automatic_qdrant].into_iter().flatten() {
            settings.validate()?;
        }
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
            || self.source_count() > MAX_SOURCES
            || self.folders.len() > folder::MAX_FOLDERS
            || self.coding_profile == Some(ProfileRef::Numeric(0))
            || self.text_profile == Some(ProfileRef::Numeric(0))
            || self.id.as_ref().is_some_and(|id| {
                id.len() != 32
                    || !id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        {
            return Err("Invalid knowledge store name or number of sources".into());
        }
        if self
            .disabled_files
            .iter()
            .chain(&self.automatic_files)
            .chain(&self.automatic_pdf_files)
            .chain(self.file_size_limits.keys())
            .chain(self.automatic_file_size_limits.keys())
            .any(|path| !self.sources.contains(path))
            || self
                .automatic_pdf_files
                .iter()
                .any(|path| !pdf::is_pdf(path))
            || self
                .disabled_folders
                .iter()
                .any(|path| !self.folders.iter().any(|folder| &folder.root == path))
        {
            return Err("A configured knowledge source is missing from the store".into());
        }
        for (path, &limit) in self
            .file_size_limits
            .iter()
            .chain(&self.automatic_file_size_limits)
        {
            validate_source_limit(path, limit)?;
        }
        for embedding in [&self.embedding, &self.automatic_embedding]
            .into_iter()
            .flatten()
        {
            if embedding.endpoint.len() > 128
                || embedding.endpoint.chars().any(char::is_control)
                || embedding.model.len() > crate::local_ai::MAX_MODEL_BYTES
                || embedding.model.chars().any(char::is_control)
                || embedding
                    .managed_model
                    .as_deref()
                    .is_some_and(|id| !super::valid_embedding_model_id(id))
            {
                return Err("Invalid knowledge store embedding settings".into());
            }
            Profile::new(&embedding.endpoint, "embedding")?;
        }
        let mut explicit = HashSet::with_capacity(self.sources.len());
        for path in &self.sources {
            saved_source_name(path).map_err(|error| error.to_string())?;
            if !explicit.insert(path) {
                return Err("A source appears more than once in a knowledge store".into());
            }
        }
        let mut paths: HashSet<_> = self
            .sources
            .iter()
            .map(|path| folder::path_key(path))
            .collect();
        for (index, folder) in self.folders.iter().enumerate() {
            folder.validate()?;
            if self.folders[..index].iter().any(|other| {
                folder::within(&folder.root, &other.root)
                    || folder::within(&other.root, &folder.root)
            }) {
                return Err("Knowledge store folders overlap".into());
            }
            for (path, _) in folder.sources() {
                if !paths.insert(folder::path_key(&path)) {
                    return Err("A source appears more than once in a knowledge store".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Scope {
    digest: [u8; 32],
    store: StoreKey,
    sources: Vec<PathBuf>,
    approved: Vec<Option<[u8; 32]>>,
    size_limits: Vec<Option<FileSizeLimit>>,
    enabled: Vec<bool>,
    pub(super) qdrant: Option<qdrant::Settings>,
    pub(super) profile: Profile,
}

#[derive(Clone, Eq, PartialEq)]
enum StoreKey {
    Id(String),
    LegacyName(String),
}

impl Scope {
    pub(super) fn new(store: &Store, endpoint: &str, model: &str) -> Result<Self, String> {
        store.validate()?;
        let profile = Profile::new(endpoint, model)?;
        let address = endpoint
            .trim()
            .parse::<std::net::SocketAddr>()
            .map_err(|error| error.to_string())?;
        let definition = (
            &store.id,
            &store.name,
            &store.sources,
            address,
            model.trim(),
        );
        let encoded = if !store.file_size_limits.is_empty() {
            serde_json::to_vec(&(definition, index_folders(store), &store.file_size_limits))
        } else if store.folders.is_empty() {
            // Keep existing explicit-file snapshots usable.
            serde_json::to_vec(&definition)
        } else {
            serde_json::to_vec(&(definition, index_folders(store)))
        };
        Self::from_definition(store, profile, encoded)
    }

    pub(super) fn managed(store: &Store, root: &Path, id: &str) -> Result<Self, String> {
        store.validate()?;
        let profile = Profile::managed_embedding(root.to_owned(), id)?;
        let definition = (
            &store.id,
            &store.name,
            &store.sources,
            ("managed-llama-embeddings", id),
            index_folders(store),
        );
        let encoded = if let Some(fingerprint) = profile.policy_authority_fingerprint() {
            serde_json::to_vec(&(definition, fingerprint, &store.file_size_limits))
        } else if store.file_size_limits.is_empty() {
            serde_json::to_vec(&definition)
        } else {
            serde_json::to_vec(&(definition, &store.file_size_limits))
        };
        Self::from_definition(store, profile, encoded)
    }

    fn from_definition(
        store: &Store,
        profile: Profile,
        encoded: Result<Vec<u8>, serde_json::Error>,
    ) -> Result<Self, String> {
        let encoded = encoded.map_err(|error| error.to_string())?;
        let base_digest = sha256(&encoded).map_err(|error| error.to_string())?;
        let digest = if let Some(settings) = &store.qdrant {
            let remote = serde_json::to_vec(&("qdrant-v1", base_digest, settings))
                .map_err(|error| error.to_string())?;
            sha256(&remote).map_err(|error| error.to_string())?
        } else {
            base_digest
        };
        let mut sources = store.sources.clone();
        let mut approved = vec![None; sources.len()];
        let mut size_limits = sources
            .iter()
            .map(|path| store.file_limit(path))
            .collect::<Vec<_>>();
        let mut enabled = sources
            .iter()
            .map(|path| store.source_enabled(SourceKind::File, path))
            .collect::<Vec<_>>();
        for folder in &store.folders {
            let include = store.source_enabled(SourceKind::Folder, &folder.root);
            for (path, digest) in folder.sources() {
                sources.push(path);
                approved.push(Some(digest));
                size_limits.push(folder.options.max_file_bytes);
                enabled.push(include);
            }
        }
        Ok(Self {
            digest,
            store: store
                .id
                .clone()
                .map_or_else(|| StoreKey::LegacyName(store.name.clone()), StoreKey::Id),
            sources,
            approved,
            size_limits,
            enabled,
            qdrant: store.qdrant.clone(),
            profile,
        })
    }

    #[cfg(test)]
    fn read(
        &self,
        position: usize,
        path: &Path,
        active: impl Fn() -> bool,
    ) -> Result<SourceDocument, String> {
        let source = read_source_capped(path, self.size_limits[position], active).map_err(|error| {
            format!("A knowledge source is unavailable ({}: {error}). Review its folder again or update the index.", path.display())
        })?;
        self.require_approved(position, path, source.digest)?;
        Ok(source)
    }

    fn require_approved(
        &self,
        position: usize,
        path: &Path,
        digest: [u8; 32],
    ) -> Result<(), String> {
        if self.approved[position].is_some_and(|approved| approved != digest) {
            return Err(format!(
                "A reviewed folder file changed ({}). Review the folder again.",
                path.display()
            ));
        }
        Ok(())
    }

    pub(super) fn same_as(&self, index: &Index) -> bool {
        self.digest == index.scope
    }

    pub(super) fn same_snapshot(&self, other: &Self) -> bool {
        self.digest == other.digest
    }

    pub(super) fn can_reuse(&self, previous: &Self) -> bool {
        self.store == previous.store
            && self.profile == previous.profile
            && self.qdrant == previous.qdrant
    }

    pub(super) fn enabled_sources(&self) -> SourceMask {
        let mut sources = SourceMask::default();
        for (index, enabled) in self.enabled.iter().enumerate() {
            if *enabled {
                sources.insert(index);
            }
        }
        sources
    }
}

fn index_folders(store: &Store) -> Vec<folder::Folder> {
    store
        .folders
        .iter()
        .map(folder::Folder::index_definition)
        .collect()
}

#[derive(Clone)]
struct Passage {
    source: usize,
    location: Location,
    text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Location {
    Line(usize),
    Page(usize),
}

impl fmt::Display for Location {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Line(line) => write!(output, "line {line}"),
            Self::Page(page) => write!(output, "page {page}"),
        }
    }
}

type Entry = (Passage, Vec<f32>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct QdrantReceipt {
    pub(super) generation: qdrant::Generation,
    pub(super) sources: SourceMask,
}

pub(crate) struct Index {
    scope: [u8; 32],
    sources: Vec<Option<[u8; 32]>>,
    entries: Vec<Entry>,
    dimensions: usize,
    graph: Option<hnsw::Graph>,
    pub(super) qdrant_receipt: Option<QdrantReceipt>,
    source_ranges: Vec<Range<usize>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct UpdateSummary {
    pub(super) reused: usize,
    pub(super) indexed: usize,
    pub(super) removed: usize,
}

impl fmt::Debug for Index {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Index")
            .field("passages", &self.len())
            .finish_non_exhaustive()
    }
}

impl Index {
    fn from_parts(
        scope: [u8; 32],
        sources: Vec<Option<[u8; 32]>>,
        entries: Vec<Entry>,
        dimensions: usize,
        graph: Option<hnsw::Graph>,
    ) -> io::Result<Self> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid knowledge index entries",
            )
        };
        if entries.len() > MAX_INDEX_PASSAGES
            || sources.len() > MAX_SOURCES
            || entries.is_empty() != (dimensions == 0)
            || dimensions > MAX_DIMENSIONS
            || graph
                .as_ref()
                .is_some_and(|graph| graph.len() != entries.len())
            || entries.iter().any(|(_, vector)| vector.len() != dimensions)
        {
            return Err(invalid());
        }
        let mut position = 0;
        let mut source_ranges = Vec::with_capacity(sources.len());
        for (source, digest) in sources.iter().enumerate() {
            let start = position;
            while entries
                .get(position)
                .is_some_and(|(passage, _)| passage.source == source)
            {
                position += 1;
            }
            if position != start && digest.is_none() {
                return Err(invalid());
            }
            source_ranges.push(start..position);
        }
        if position != entries.len() {
            return Err(invalid());
        }
        Ok(Self {
            scope,
            sources,
            entries,
            dimensions,
            graph,
            qdrant_receipt: None,
            source_ranges,
        })
    }

    fn source_entries(&self, source: usize) -> &[Entry] {
        &self.entries[self.source_ranges[source].clone()]
    }

    fn enabled_entries(&self, scope: &Scope) -> usize {
        scope
            .enabled
            .iter()
            .zip(&self.source_ranges)
            .filter(|(enabled, _)| **enabled)
            .map(|(_, range)| range.len())
            .sum()
    }

    fn search_graph(&self, enabled: usize) -> Option<&hnsw::Graph> {
        // Exact ranking of a narrow selection skips the disabled ranges completely.
        self.graph
            .as_ref()
            .filter(|_| enabled > EXACT_SEARCH_LIMIT && enabled > self.entries.len() / 2)
    }

    #[cfg(test)]
    pub(super) fn build(client: &Client, scope: &Scope) -> Result<Self, String> {
        Self::update(client, scope, None, false, || Ok(())).map(|(index, _)| index)
    }

    pub(super) fn update(
        client: &Client,
        scope: &Scope,
        previous: Option<(&Scope, &Self)>,
        rebuild: bool,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<(Self, UpdateSummary), String> {
        Self::refresh(client, scope, previous, rebuild, None, authorize)
    }

    fn reconcile(
        client: &Client,
        scope: &Scope,
        previous: Option<(&Scope, &Self)>,
        expanded_qdrant: bool,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<(Self, UpdateSummary), String> {
        Self::refresh(
            client,
            scope,
            previous,
            false,
            Some(expanded_qdrant),
            authorize,
        )
    }

    fn refresh(
        client: &Client,
        scope: &Scope,
        previous: Option<(&Scope, &Self)>,
        rebuild: bool,
        automatic_qdrant: Option<bool>,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<(Self, UpdateSummary), String> {
        authorize()?;
        let previous =
            previous.filter(|(base, index)| scope.can_reuse(base) && base.same_as(index));
        let mut summary = UpdateSummary::default();
        let previous_positions = previous.map(|(base, index)| {
            let mut positions = HashMap::with_capacity(base.sources.len());
            for (position, path) in base.sources.iter().enumerate() {
                positions.entry(folder::path_key(path)).or_insert(position);
            }
            (positions, index)
        });
        if let Some((base, _)) = previous {
            let current: HashSet<_> = scope
                .sources
                .iter()
                .map(|path| folder::path_key(path))
                .collect();
            summary.removed = base
                .sources
                .iter()
                .filter(|path| !current.contains(&folder::path_key(path)))
                .count();
        }
        let mut entries = Vec::new();
        let mut disabled = Vec::new();
        let header = snapshot::V4_BASE_BYTES
            + 33 * scope.sources.len()
            + scope
                .qdrant
                .as_ref()
                .map_or(0, |_| snapshot::qdrant_receipt_bytes(scope.sources.len()));
        let mut payload = header;
        let mut sources = Vec::with_capacity(scope.sources.len());
        let legacy_qdrant = scope.qdrant.is_some() && automatic_qdrant == Some(false);
        let max_passages = if legacy_qdrant {
            MAX_CHUNKS
        } else {
            MAX_INDEX_PASSAGES
        };
        for (source, path) in scope.sources.iter().enumerate() {
            check_active(client)?;
            let previous_source = previous_positions.as_ref().and_then(|(positions, index)| {
                Some((*positions.get(&folder::path_key(path))?, *index))
            });
            if !scope.enabled[source] {
                sources.push(previous_source.and_then(|(old, index)| index.sources[old]));
                if let Some((old, index)) = previous_source {
                    disabled.push((source, index.source_entries(old)));
                }
                continue;
            }
            let start = entries.len();
            let summary_source = scan_source(
                path,
                scope.size_limits[source],
                || client.active(),
                |location, text| {
                    if legacy_qdrant
                        && matches!(location, Location::Line(line) if line > source::PREVIOUS_LARGE_TEXT_BYTES)
                    {
                        return Err(io::Error::other(
                            "Review the automatic Qdrant destination again to include this larger source.",
                        ));
                    }
                    require_passage_count(entries.len() + 1, max_passages)
                        .map_err(io::Error::other)?;
                    payload += snapshot::encoded_entry_bytes(text.len(), 0);
                    require_index_bytes(payload).map_err(io::Error::other)?;
                    entries.push((
                        Passage {
                            source,
                            location,
                            text: text.into(),
                        },
                        Vec::new(),
                    ));
                    Ok(())
                },
            );
            check_active(client)?;
            let summary_source = summary_source.map_err(|error| source_unavailable(path, error))?;
            let digest = summary_source.digest;
            scope.require_approved(source, path, digest)?;
            sources.push(Some(digest));
            let staged = &mut entries[start..];
            let cached = previous_source
                .filter(|&(old, index)| !rebuild && index.sources[old] == Some(digest))
                .map(|(old, index)| index.source_entries(old))
                .filter(|cached| {
                    cached
                        .iter()
                        .zip(staged.iter())
                        .all(|((old, _), (new, _))| {
                            old.location == new.location && old.text == new.text
                        })
                        && cached.len() == staged.len()
                });
            if let Some(cached) = cached {
                summary.reused += 1;
                for ((_, previous), (_, current)) in cached.iter().zip(staged) {
                    payload += previous.len() * size_of::<f32>();
                    require_index_bytes(payload)?;
                    *current = previous.clone();
                }
            } else {
                summary.indexed += 1;
            }
        }
        let old_dimensions = previous.map_or(0, |(_, index)| index.dimensions);
        let disabled_payload = disabled
            .iter()
            .flat_map(|(_, entries)| *entries)
            .map(|(passage, vector)| {
                snapshot::encoded_entry_bytes(passage.text.len(), vector.len())
            })
            .sum::<usize>();
        let disabled_count = disabled
            .iter()
            .map(|(_, entries)| entries.len())
            .sum::<usize>();
        let keep_disabled = |dimensions| old_dimensions == 0 || old_dimensions == dimensions;
        let enabled_count = entries.len();
        let collection_dimensions = scope.qdrant.as_ref().map(|settings| settings.dimensions);
        let dimensions = if entries.is_empty() {
            if automatic_qdrant.is_none() {
                return Err("The source files contain no text".into());
            }
            if disabled_count == 0 {
                0
            } else {
                old_dimensions
            }
        } else {
            embed_missing(
                client,
                &mut entries,
                collection_dimensions,
                &authorize,
                |dimensions, projected| {
                    if collection_dimensions.is_some_and(|expected| expected != dimensions) {
                        return Err("The embedding dimension does not match the saved Qdrant collection settings.".into());
                    }
                    let retained = if keep_disabled(dimensions) {
                        require_passage_count(enabled_count + disabled_count, max_passages)?;
                        disabled_payload
                    } else {
                        0
                    };
                    require_index_bytes(header + projected + retained)
                },
            )?
        };
        if !keep_disabled(dimensions)
            && sources
                .iter()
                .zip(&scope.enabled)
                .any(|(digest, enabled)| !enabled && digest.is_some())
        {
            if !rebuild {
                return Err("The embedding dimensions changed. Choose Rebuild index to replace all vectors.".into());
            }
            for (digest, enabled) in sources.iter_mut().zip(&scope.enabled) {
                if !enabled {
                    *digest = None;
                }
            }
        } else if disabled_count != 0 {
            require_passage_count(entries.len() + disabled_count, max_passages)?;
            for (source, saved) in disabled {
                entries.extend(saved.iter().map(|(passage, vector)| {
                    (
                        Passage {
                            source,
                            ..passage.clone()
                        },
                        vector.clone(),
                    )
                }));
            }
            entries.sort_by_key(|(passage, _)| passage.source);
        }
        let bytes = header
            + entries
                .iter()
                .map(|(passage, vector)| {
                    snapshot::encoded_entry_bytes(passage.text.len(), vector.len())
                })
                .sum::<usize>();
        require_index_bytes(bytes)?;
        let graph = if scope.qdrant.is_none()
            && (EXACT_SEARCH_LIMIT + 1..=MAX_CHUNKS).contains(&entries.len())
        {
            let reusable = previous
                .filter(|(base, index)| {
                    !rebuild
                        && summary.indexed == 0
                        && base.same_snapshot(scope)
                        && index.dimensions == dimensions
                        && index.entries.len() == entries.len()
                })
                .and_then(|(_, index)| index.graph.clone());
            Some(match reusable {
                Some(graph) => graph,
                None => hnsw::Graph::build(&entries, || check_active(client))?,
            })
        } else {
            None
        };
        require_index_bytes(bytes + graph.as_ref().map_or(0, hnsw::Graph::encoded_bytes))?;
        let index = Self::from_parts(scope.digest, sources, entries, dimensions, graph)
            .map_err(|error| error.to_string())?;
        authorize()?;
        index.check_sources(scope, || check_active(client))?;
        Ok((index, summary))
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>()
            + self.sources.capacity() * size_of::<Option<[u8; 32]>>()
            + self.source_ranges.capacity() * size_of::<Range<usize>>()
            + self
                .entries
                .iter()
                .map(|(passage, vector)| {
                    vector.capacity() * size_of::<f32>() + passage.text.capacity()
                })
                .sum::<usize>()
            + self.graph.as_ref().map_or(0, hnsw::Graph::bytes)
    }

    #[cfg(test)]
    fn search(
        &self,
        client: &Client,
        scope: &Scope,
        query: &str,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<String>, String> {
        self.search_with_vault(client, scope, query, None, authorize)
    }

    pub(super) fn search_with_vault(
        &self,
        client: &Client,
        scope: &Scope,
        query: &str,
        vault_key: Option<Arc<qdrant::VaultKey>>,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<String>, String> {
        let vector = self.query_vector(client, scope, query, vault_key.as_deref(), &authorize)?;
        let ranked = self.rank_reviewed(
            client,
            scope,
            &vector,
            SEARCH_HITS,
            vault_key.clone(),
            &authorize,
        )?;
        if let Some(settings) = &scope.qdrant {
            require_source_digests(scope, &self.sources, || check_active(client))?;
            authorize()?;
            settings.require_vault_key(vault_key.as_deref())?;
        }
        Ok(ranked
            .into_iter()
            .map(|(score, index)| {
                let (passage, _) = &self.entries[index];
                format!(
                    "{} · {} · {score:.3}\n{}",
                    scope.sources[passage.source].display(),
                    passage.location,
                    passage.text
                )
            })
            .collect())
    }

    fn query_vector(
        &self,
        client: &Client,
        scope: &Scope,
        query: &str,
        vault_key: Option<&qdrant::VaultKey>,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<f32>, String> {
        if query.trim().is_empty() {
            return Err("Enter a search query".into());
        }
        let check = || {
            check_active(client)?;
            if let Some(settings) = &scope.qdrant {
                settings.require_vault_key(vault_key)?;
            }
            Ok(())
        };
        if scope.qdrant.is_some() {
            authorize()?;
            remote_index::require_policy()?;
            remote_index::require_coverage(self, scope)?;
        }
        self.check_sources(scope, check)?;
        if self.enabled_entries(scope) == 0 {
            return Err("No enabled knowledge sources contain indexed text.".into());
        }
        check()?;
        authorize()?;
        let mut embeddings = client.embed_reviewed(&[query], || {
            check()?;
            authorize()?;
            if scope.qdrant.is_some() {
                remote_index::require_policy()?;
            }
            Ok(())
        })?;
        let [vector] = embeddings.as_mut_slice() else {
            return Err("The service returned the wrong number of query vectors".into());
        };
        normalize(vector, self.dimensions)?;
        require_source_digests(scope, &self.sources, check)?;
        Ok(std::mem::take(vector))
    }

    fn rank_reviewed(
        &self,
        client: &Client,
        scope: &Scope,
        vector: &[f32],
        limit: usize,
        vault_key: Option<Arc<qdrant::VaultKey>>,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<(f32, usize)>, String> {
        if scope.qdrant.is_some() {
            remote_index::query(client, scope, self, vector, limit, vault_key, authorize)
        } else {
            self.rank_candidates(scope, vector, limit, || check_active(client))
        }
    }

    fn rank_candidates(
        &self,
        scope: &Scope,
        vector: &[f32],
        limit: usize,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<(f32, usize)>, String> {
        let enabled = self.enabled_entries(scope);
        if let Some(graph) = self.search_graph(enabled)
            && limit < enabled
        {
            let candidates = graph.search(&self.entries, vector, &scope.enabled, limit, &check)?;
            if candidates.len() == limit {
                return Ok(candidates);
            }
        }
        let mut exact = self.rank_exact(scope, vector, check)?;
        exact.truncate(limit);
        Ok(exact)
    }

    fn rank_exact(
        &self,
        scope: &Scope,
        vector: &[f32],
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<(f32, usize)>, String> {
        let mut ranked = Vec::with_capacity(self.enabled_entries(scope));
        for (enabled, range) in scope.enabled.iter().zip(&self.source_ranges) {
            if *enabled {
                for position in range.clone() {
                    check()?;
                    ranked.push((score(vector, &self.entries[position].1), position));
                }
            }
        }
        ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        Ok(ranked)
    }

    pub(super) fn check_sources(
        &self,
        scope: &Scope,
        check_active: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        if !scope.same_as(self) || scope.sources.len() != self.sources.len() {
            return Err(
                "The knowledge index no longer matches this store. Update the index first.".into(),
            );
        }
        for (position, (path, expected)) in scope.sources.iter().zip(&self.sources).enumerate() {
            check_active()?;
            if !scope.enabled[position] {
                continue;
            }
            let Some(expected) = expected else {
                return Err(
                    "An enabled knowledge source is missing from the index. Choose Update index."
                        .into(),
                );
            };
            let mut stored = self.source_entries(position).iter();
            let mut passages_match = true;
            let document = scan_source(
                path,
                scope.size_limits[position],
                || check_active().is_ok(),
                |location, text| {
                    passages_match &= stored.next().is_some_and(|(passage, _)| {
                        passage.location == location && passage.text == text
                    });
                    Ok(())
                },
            );
            check_active()?;
            let document = document.map_err(|error| source_unavailable(path, error))?;
            scope.require_approved(position, path, document.digest)?;
            if document.digest != *expected {
                return Err(format!(
                    "A knowledge source changed ({}). Update the index first.",
                    path.display()
                ));
            }
            if !passages_match || stored.next().is_some() {
                return Err(
                    "The saved passages do not match the source. Update the index first.".into(),
                );
            }
        }
        check_active()
    }
}

fn source_unavailable(path: &Path, error: io::Error) -> String {
    format!(
        "A knowledge source is unavailable ({}: {error}). Review its folder again or update the index.",
        path.display()
    )
}

fn require_source_digests(
    scope: &Scope,
    digests: &[Option<[u8; 32]>],
    check_active: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (position, (path, expected)) in scope.sources.iter().zip(digests).enumerate() {
        check_active()?;
        if !scope.enabled[position] {
            continue;
        }
        let digest = read_source_digest(path, scope.size_limits[position], &check_active)?;
        scope.require_approved(position, path, digest)?;
        if Some(digest) != *expected {
            return Err(format!(
                "A knowledge source changed ({}). Update the index and review the request again.",
                path.display()
            ));
        }
    }
    check_active()
}

fn embed_missing(
    client: &Client,
    entries: &mut [Entry],
    dimensions_hint: Option<usize>,
    authorize: impl Fn() -> Result<(), String>,
    budget: impl Fn(usize, usize) -> Result<(), String>,
) -> Result<usize, String> {
    let entry_count = entries.len();
    let text_bytes = entries
        .iter()
        .map(|(passage, _)| snapshot::encoded_entry_bytes(passage.text.len(), 0))
        .sum::<usize>();
    let check_budget = |dimensions| {
        if !(1..=MAX_DIMENSIONS).contains(&dimensions) {
            return Err("The embedding dimensions are invalid or changed".into());
        }
        budget(
            dimensions,
            text_bytes + entry_count * dimensions * size_of::<f32>(),
        )
    };
    let mut dimensions = entries
        .iter()
        .find_map(|(_, vector)| (!vector.is_empty()).then_some(vector.len()));
    let reusing = dimensions.is_some();
    if let Some(dimensions) = dimensions.or(dimensions_hint) {
        check_budget(dimensions)?;
    }
    let mut pending = entries.iter_mut().filter(|(_, vector)| vector.is_empty());
    loop {
        let batch: Vec<_> = pending.by_ref().take(BATCH_SIZE).collect();
        if batch.is_empty() {
            break;
        }
        check_active(client)?;
        authorize()?;
        let input: Vec<_> = batch
            .iter()
            .map(|(passage, _)| passage.text.as_str())
            .collect();
        let embeddings = client.embed(&input)?;
        if embeddings.len() != batch.len() {
            return Err("The service returned the wrong number of embedding vectors".into());
        }
        for ((_, target), mut vector) in batch.into_iter().zip(embeddings) {
            if dimensions.is_none() {
                check_budget(vector.len())?;
            }
            let expected = *dimensions.get_or_insert(vector.len());
            if reusing && expected != vector.len() {
                return Err("The embedding dimensions changed. Choose Rebuild index to replace all vectors.".into());
            }
            normalize(&mut vector, expected)?;
            *target = vector;
        }
    }
    dimensions.ok_or_else(|| "The embedding service returned no vectors".into())
}

fn require_passage_count(count: usize, limit: usize) -> Result<(), String> {
    if count > limit {
        Err(format!(
            "A knowledge index supports up to {limit} text passages"
        ))
    } else {
        Ok(())
    }
}

fn require_index_bytes(bytes: usize) -> Result<(), String> {
    if bytes as u64 > snapshot::MAX_BYTES {
        Err(format!(
            "The knowledge index exceeds {} MiB. Select fewer files or an embedding model with fewer dimensions.",
            snapshot::MAX_BYTES / (1024 * 1024)
        ))
    } else {
        Ok(())
    }
}

fn sha256(bytes: &[u8]) -> io::Result<[u8; 32]> {
    HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(io::Error::other)?
        .try_into()
        .map_err(|_| io::Error::other("Invalid SHA-256 digest"))
}

fn source_name(path: &Path) -> io::Result<&str> {
    let safe = path.is_absolute()
        && !path.components().any(|part| part == Component::ParentDir)
        && path
            .to_str()
            .is_some_and(|value| value.len() <= 4096 && !value.chars().any(char::is_control));
    if safe && let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        return Ok(name);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Enter an absolute path to a knowledge source",
    ))
}

/// Keep source metadata copied from another platform visible in AI Manager.
/// The runtime still uses `source_name` and requires a native absolute path.
fn saved_source_name(path: &Path) -> io::Result<&str> {
    source_name(path).or_else(|error| {
        let text = path
            .to_str()
            .filter(|text| text.len() <= 4096 && !text.chars().any(char::is_control))
            .ok_or_else(|| io::Error::new(error.kind(), "Invalid saved knowledge source path"))?;
        let tail = if cfg!(windows) {
            text.strip_prefix('/').filter(|tail| !tail.starts_with('/'))
        } else if matches!(text.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic()) {
            text.get(3..)
        } else if let Some(unc) = text.strip_prefix(r"\\") {
            unc.split_once(['\\', '/'])
                .and_then(|(host, tail)| (!host.is_empty()).then_some(tail))
                .and_then(|tail| tail.split_once(['\\', '/']))
                .and_then(|(share, tail)| (!share.is_empty()).then_some(tail))
        } else {
            None
        };
        let mut name = None;
        for component in tail.ok_or(error)?.split(['\\', '/']) {
            if matches!(component, "" | "." | "..") {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid saved knowledge source path"));
            }
            name = Some(component);
        }
        name.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid saved knowledge source path"))
    })
}

struct SourceDocument {
    text: SourceText,
    digest: [u8; 32],
    bytes: usize,
}

struct SourceSummary {
    digest: [u8; 32],
    bytes: usize,
    has_passages: bool,
}

enum SourceText {
    Text(String),
    Pages(Vec<String>),
}

impl SourceDocument {
    fn passages(&self) -> impl Iterator<Item = (Location, &str)> {
        let (text, pages) = match &self.text {
            SourceText::Text(text) => (text.as_str(), &[][..]),
            SourceText::Pages(pages) => ("", pages.as_slice()),
        };
        chunks(text)
            .map(|(line, text)| (Location::Line(line), text))
            .chain(pages.iter().enumerate().flat_map(|(index, page)| {
                chunks(page).map(move |(_, text)| (Location::Page(index + 1), text))
            }))
    }
}

#[cfg(test)]
fn read_source_capped(
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
    active: impl Fn() -> bool,
) -> io::Result<SourceDocument> {
    with_source_file(
        path,
        |error| error,
        |file| read_source_file_capped(file, path, max_file_bytes, active),
    )
}

fn scan_source(
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
    active: impl Fn() -> bool,
    passage: impl FnMut(Location, &str) -> io::Result<()>,
) -> io::Result<SourceSummary> {
    with_source_file(
        path,
        |error| error,
        |file| scan_source_file(file, path, max_file_bytes, active, passage),
    )
}

fn scan_source_file(
    file: &File,
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
    active: impl Fn() -> bool,
    mut passage: impl FnMut(Location, &str) -> io::Result<()>,
) -> io::Result<SourceSummary> {
    if !pdf::is_pdf(path) {
        let format_limit = source::format_file_size(path, max_file_bytes);
        let configured = max_file_bytes.is_some_and(|limit| limit.bytes() <= format_limit as u64);
        return text_stream::scan(
            file,
            source::effective_file_size(path, max_file_bytes),
            active,
            passage,
        )
        .map_err(|error| {
            if !configured
                && error
                    .get_ref()
                    .is_some_and(|cause| cause.is::<ConfiguredFileTooLarge>())
            {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Text file exceeds {} KiB", format_limit / 1024),
                )
            } else {
                error
            }
        });
    }
    let document = read_source_file_capped(file, path, max_file_bytes, &active)?;
    let mut has_passages = false;
    for (location, text) in document.passages() {
        if !active() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "The knowledge request was cancelled",
            ));
        }
        passage(location, text)?;
        has_passages = true;
    }
    Ok(SourceSummary {
        digest: document.digest,
        bytes: document.bytes,
        has_passages,
    })
}

fn with_source_file<T, E>(
    path: &Path,
    map_error: impl Fn(io::Error) -> E,
    read: impl FnOnce(&File) -> Result<T, E>,
) -> Result<T, E> {
    let file = open_source(path).map_err(&map_error)?;
    let stamp = FileStamp::read(&file).map_err(&map_error)?;
    let source = read(&file)?;
    stamp.require_unchanged(&file).map_err(&map_error)?;
    stamp
        .require_unchanged(&open_source(path).map_err(&map_error)?)
        .map_err(map_error)?;
    Ok(source)
}

fn read_source_file_capped(
    file: &File,
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
    active: impl Fn() -> bool,
) -> io::Result<SourceDocument> {
    if pdf::is_pdf(path) {
        let source = read_pdf(file, path, max_file_bytes)?;
        let bytes = source.len();
        let digest = sha256(&source)?;
        let pages = pdf::extract(source, active)?;
        Ok(SourceDocument {
            text: SourceText::Pages(pages),
            digest,
            bytes,
        })
    } else {
        let text = read_text(file, path, max_file_bytes)?;
        let digest = sha256(text.as_bytes())?;
        let bytes = text.len();
        Ok(SourceDocument {
            text: SourceText::Text(text),
            digest,
            bytes,
        })
    }
}

fn read_source_digest(
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
    check: impl Fn() -> Result<(), String>,
) -> Result<[u8; 32], String> {
    // The recorded digest came from a validated source. Exact equality also
    // preserves its UTF-8 or PDF content, so a recheck need only hash bytes.
    let limit = source::effective_file_size(path, max_file_bytes);
    let unavailable = |error: &dyn fmt::Display| {
        format!(
            "A knowledge source is unavailable ({}: {error}).",
            path.display()
        )
    };
    let io_error = |error: io::Error| unavailable(&error);
    with_source_file(path, io_error, |file| {
        if file.metadata().map_err(io_error)?.len() > limit {
            return Err(unavailable(&ConfiguredFileTooLarge));
        }
        let mut reader = file.take(limit + 1);
        let mut hash = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(|error| unavailable(&error))?;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            check()?;
            let count = reader.read(&mut buffer).map_err(io_error)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if reader.limit() == 0 {
            return Err(unavailable(&ConfiguredFileTooLarge));
        }
        Ok(hash
            .finalize()
            .as_ref()
            .try_into()
            .expect("SHA-256 has 32 bytes"))
    })
}

fn read_pdf(
    file: &File,
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
) -> io::Result<Vec<u8>> {
    let format_limit = source::format_file_size(path, max_file_bytes);
    if let Some(bytes) = read_reviewed_bytes(file, max_file_bytes, format_limit)? {
        pdf::validate_bytes(&bytes)?;
        Ok(bytes)
    } else {
        pdf::read_bytes(file)
    }
}

fn read_text(
    file: &File,
    path: &Path,
    max_file_bytes: Option<FileSizeLimit>,
) -> io::Result<String> {
    let format_limit = source::format_file_size(path, max_file_bytes);
    let source = if let Some(bytes) = read_reviewed_bytes(file, max_file_bytes, format_limit)? {
        String::from_utf8(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
    } else {
        let mut source = String::new();
        file.take((format_limit + 1) as u64)
            .read_to_string(&mut source)?;
        if source.len() > format_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Text file exceeds {} KiB", format_limit / 1024),
            ));
        }
        source
    };
    if !valid_text(&source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Source contains binary control characters",
        ));
    }
    Ok(source)
}

#[derive(Debug)]
struct ConfiguredFileTooLarge;

impl ConfiguredFileTooLarge {
    const MESSAGE: &str = "File exceeds the configured size limit";
}

impl fmt::Display for ConfiguredFileTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(Self::MESSAGE)
    }
}

impl std::error::Error for ConfiguredFileTooLarge {}

fn read_reviewed_bytes(
    file: &File,
    max_file_bytes: Option<FileSizeLimit>,
    format_limit: usize,
) -> io::Result<Option<Vec<u8>>> {
    let Some(limit) = max_file_bytes
        .map(FileSizeLimit::bytes)
        .filter(|&limit| limit <= format_limit as u64)
    else {
        return Ok(None);
    };
    let length = file.metadata()?.len();
    if length > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            ConfiguredFileTooLarge,
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            ConfiguredFileTooLarge,
        ));
    }
    Ok(Some(bytes))
}

fn validate_source_limit(path: &Path, limit: FileSizeLimit) -> Result<(), String> {
    limit.validate()?;
    if !pdf::is_pdf(path) && limit.is_reviewed() && limit.bytes() > source::MAX_TEXT_BYTES as u64 {
        return Err(format!(
            "Enter a maximum text file size up to {} bytes.",
            source::MAX_TEXT_BYTES
        ));
    }
    Ok(())
}

fn valid_text(text: &str) -> bool {
    !text
        .chars()
        .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
}

fn open_source(path: &Path) -> io::Result<File> {
    let name = source_name(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Source has no parent directory"))?;
    Directory::open(parent, false)?.open_file(name)
}

fn chunks(mut input: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut line = 1;
    std::iter::from_fn(move || {
        while !input.is_empty() {
            let (prefix, rest) = input.split_at(chunk_end(input));
            input = rest;
            let start = line;
            line += prefix.bytes().filter(|&byte| byte == b'\n').count();
            if !prefix.trim().is_empty() {
                return Some((start, prefix.trim_end()));
            }
        }
        None
    })
}

fn chunk_end(input: &str) -> usize {
    let boundary = input.floor_char_boundary(CHUNK_BYTES.min(input.len()));
    if boundary < input.len() {
        input[..boundary]
            .rfind('\n')
            .map_or(boundary, |offset| offset + 1)
    } else {
        boundary
    }
}

pub(super) fn check_active(client: &Client) -> Result<(), String> {
    if client.active() {
        Ok(())
    } else {
        Err("The knowledge request was cancelled".into())
    }
}

fn normalize(vector: &mut [f32], expected: usize) -> Result<(), String> {
    if !(1..=MAX_DIMENSIONS).contains(&expected) || vector.len() != expected {
        return Err("The embedding dimensions are invalid or changed".into());
    }
    let sum: f64 = vector.iter().map(|&value| f64::from(value).powi(2)).sum();
    if !sum.is_finite() || sum == 0.0 {
        return Err("The service returned an invalid embedding vector".into());
    }
    let length = sum.sqrt();
    for value in vector {
        *value = (f64::from(*value) / length) as f32;
    }
    Ok(())
}

fn score(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::sync::Arc;

    use serde_json::{Value, json};

    use crate::local_ai::test_support;

    use super::super::{Dialog, Message, State, tests::profile_catalog};
    use super::*;

    #[test]
    fn index_refuses_more_than_the_passage_limit_before_embedding() {
        let fixture = crate::snippets::tests::Profile::new();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let mut store = Store::new("Passage limit fixture").unwrap();
        // End every chunk at a newline and cross the local corpus limit with
        // two explicitly reviewed files.
        let mut full = vec![b'x'; MAX_INDEX_PASSAGES / 2 * CHUNK_BYTES];
        for end in (CHUNK_BYTES - 1..full.len()).step_by(CHUNK_BYTES) {
            full[end] = b'\n';
        }
        for file in ["first.txt", "second.txt"] {
            let path = fixture.0.join(file);
            fs::write(&path, &full).unwrap();
            store.add_source(path.clone()).unwrap();
            store
                .set_file_size_limit(path, Some(full.len() as u64))
                .unwrap();
        }
        drop(full);
        let extra = fixture.0.join("extra.txt");
        fs::write(&extra, "One more passage").unwrap();
        store.add_source(extra).unwrap();
        let scope = Scope::new(&store, &endpoint, "fixture:embedding").unwrap();
        let error = Index::build(&Client::new(scope.profile.clone()), &scope).unwrap_err();
        assert!(
            error.contains(&format!(
                "supports up to {MAX_INDEX_PASSAGES} text passages"
            )),
            "{error}"
        );
        let mut appended = std::fs::OpenOptions::new()
            .append(true)
            .open(&scope.sources[0])
            .unwrap();
        appended.write_all(b"x").unwrap();
        drop(appended);
        let error = Index::build(&Client::new(scope.profile.clone()), &scope).unwrap_err();
        assert!(error.contains(ConfiguredFileTooLarge::MESSAGE), "{error}");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn qdrant_dimensions_refuse_an_oversized_snapshot_before_embedding() {
        let fixture = crate::snippets::tests::Profile::new();
        let (embedding, _) = test_support::listener();
        let (qdrant, _) = test_support::listener();
        let endpoint = embedding.local_addr().unwrap().to_string();
        let mut store = Store::new("Qdrant budget fixture").unwrap();
        store.qdrant = Some(qdrant::Settings {
            endpoint: format!("http://{}", qdrant.local_addr().unwrap()),
            collection: "fixture".into(),
            dimensions: MAX_DIMENSIONS,
            ..qdrant::Settings::default()
        });
        let path = fixture.0.join("source.txt");
        let text = vec![b'x'; 6 * 1024 * 1024];
        fs::write(&path, &text).unwrap();
        store.add_source(path.clone()).unwrap();
        store
            .set_file_size_limit(path, Some(text.len() as u64))
            .unwrap();
        let scope = Scope::new(&store, &endpoint, "fixture:embedding").unwrap();
        let error = Index::build(&Client::new(scope.profile.clone()), &scope).unwrap_err();
        assert!(error.contains("exceeds 96 MiB"), "{error}");
        for listener in [embedding, qdrant] {
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn explicit_file_size_limit_survives_reopen_and_restricts_index_reads() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("text.txt");
        fs::write(&path, "éé").unwrap();
        let mut store = Store::new("Bounded source").unwrap();
        store.add_source(path.clone()).unwrap();
        store.set_file_sync(path.clone(), source::SyncMode::Automatic);
        let saved = State {
            stores: vec![store],
            ..State::default()
        };
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let legacy = Scope::new(&saved.stores[0], "127.0.0.1:5678", "fixture").unwrap();
        for event in [
            Message::SelectStore(0),
            Message::ConfigureFileSize(0, path.clone()),
            Message::FileSize("0".into()),
            Message::ApplyFileSize(0, path.clone()),
        ] {
            let _ = dialog.update(event, &mut draft, &saved, false);
        }
        assert_eq!(draft.stores[0].file_size_limit(&path), None);
        assert_eq!(
            draft.stores[0].file_sync(&path),
            source::SyncMode::Automatic
        );
        for event in [
            Message::FileSize("4".into()),
            Message::ApplyFileSize(0, path.clone()),
        ] {
            let _ = dialog.update(event, &mut draft, &saved, false);
        }
        let store = &mut draft.stores[0];
        assert_eq!(store.file_size_limit(&path), Some(4));
        assert_eq!(store.file_sync(&path), source::SyncMode::Manual);
        assert!(store.set_file_sync(path.clone(), source::SyncMode::Automatic));
        let stored = serde_json::to_vec(store).unwrap();
        let mut reopened: Store = serde_json::from_slice(&stored).unwrap();
        reopened.validate().unwrap();
        assert_eq!(reopened.file_size_limit(&path), Some(4));
        assert_eq!(reopened.file_sync(&path), source::SyncMode::Automatic);

        let scope = Scope::new(&reopened, "127.0.0.1:5678", "fixture").unwrap();
        assert!(!scope.same_snapshot(&legacy));
        let document = scope.read(0, &path, || true).unwrap();
        assert_eq!(document.bytes, 4);
        require_source_digests(&scope, &[Some(document.digest)], || Ok(())).unwrap();
        fs::write(&path, "ééx").unwrap();
        assert!(scope.read(0, &path, || true).is_err());
        assert!(require_source_digests(&scope, &[Some(document.digest)], || Ok(())).is_err());

        reopened
            .file_size_limits
            .insert(path.clone(), FileSizeLimit::Legacy(5));
        assert_eq!(reopened.file_sync(&path), source::SyncMode::Manual);
        assert!(reopened.set_file_size_limit(path, None).unwrap());
        let defaults = Scope::new(&reopened, "127.0.0.1:5678", "fixture").unwrap();
        assert!(defaults.same_snapshot(&legacy));
        let value = serde_json::to_value(&reopened).unwrap();
        assert!(value.get("file_size_limits").is_none());
        assert!(value.get("automatic_file_size_limits").is_none());
    }

    #[test]
    fn explicitly_expanded_text_keeps_legacy_approval_and_large_line_citations_distinct() {
        let _exclusive = test_support::exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("text.txt");
        let leading_lines = source::DEFAULT_TEXT_BYTES + CHUNK_BYTES;
        let evidence = "The deep source line is searchable.";
        let text = format!("{}{evidence}", "\n".repeat(leading_lines));
        let size = text.len() as u64;
        fs::write(&path, text).unwrap();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let mut store = Store::new("Expanded source").unwrap();
        store.add_source(path.clone()).unwrap();
        store
            .file_size_limits
            .insert(path.clone(), FileSizeLimit::Legacy(size));
        assert!(store.set_file_sync(path.clone(), source::SyncMode::Automatic));
        let persisted = serde_json::to_value(&store).unwrap();
        assert_eq!(persisted["file_size_limits"][path.to_str().unwrap()], size);
        let mut store: Store = serde_json::from_value(persisted).unwrap();
        let legacy = Scope::new(&store, &endpoint, "fixture").unwrap();
        assert_eq!(store.file_sync(&path), source::SyncMode::Automatic);
        assert_eq!(
            store.effective_file_size_limit(&path),
            source::DEFAULT_TEXT_BYTES as u64
        );
        assert!(legacy.read(0, &path, || true).is_err());
        assert!(store.set_file_size_limit(path.clone(), Some(size)).unwrap());
        assert_eq!(store.file_sync(&path), source::SyncMode::Manual);
        assert!(store.set_file_sync(path.clone(), source::SyncMode::Automatic));
        let persisted = serde_json::to_value(&store).unwrap();
        assert_eq!(
            persisted["file_size_limits"][path.to_str().unwrap()],
            json!({"bytes": size})
        );
        let store: Store = serde_json::from_value(persisted).unwrap();
        assert_eq!(store.file_sync(&path), source::SyncMode::Automatic);
        let scope = Scope::new(&store, &endpoint, "fixture").unwrap();
        assert!(!scope.same_snapshot(&legacy));
        assert_eq!(scope.read(0, &path, || true).unwrap().bytes, size as usize);

        let server = std::thread::spawn(move || {
            for expected in [evidence, "What is searchable?"] {
                let mut socket = test_support::accept(&listener);
                let request: Value = serde_json::from_slice(&test_support::wire_request(
                    &mut socket,
                    "POST /api/embed HTTP/1.1",
                ))
                .unwrap();
                assert_eq!(request["input"], json!([expected]));
                let response =
                    test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1,0]]}"#);
                socket.get_mut().write_all(response.as_bytes()).unwrap();
            }
        });
        let client = Client::new(scope.profile.clone());
        let index = Index::build(&client, &scope).unwrap();
        let cache = Cache::new(profile.0.clone());
        cache.save(&index, || Ok(())).unwrap();
        let reopened = cache.load(&scope).unwrap();
        assert_eq!(
            reopened.entries[0].0.location,
            Location::Line(leading_lines + 1)
        );
        let plan = retrieval::Plan::new(
            "Expanded source".into(),
            endpoint,
            "fixture".into(),
            scope,
            profile.0.clone(),
        );
        let pending = plan
            .start("What is searchable?".into(), 16_384, None)
            .unwrap();
        let retrieved = pending.reply.recv_blocking().unwrap().unwrap();
        retrieved.require_current(&client).unwrap();
        assert!(retrieved.context().contains(evidence));
        let citations = serde_json::to_value(retrieved.citations()).unwrap();
        assert_eq!(citations[0]["line"], leading_lines + 1);
        let restored: Vec<retrieval::Citation> = serde_json::from_value(citations).unwrap();
        assert_eq!(restored, retrieved.citations());
        server.join().unwrap();
    }

    #[test]
    fn expanded_pdf_uses_the_same_size_boundary_for_extraction_and_digests() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("large.pdf");
        let bytes = pdf::large_fixture(pdf::DEFAULT_FILE_BYTES + 1);
        let size = bytes.len() as u64;
        fs::write(&path, &bytes).unwrap();
        let mut store = Store::new("Expanded PDF").unwrap();
        store.add_source(path.clone()).unwrap();
        let scope = |store: &Store| Scope::new(store, "127.0.0.1:5678", "fixture").unwrap();
        assert!(scope(&store).read(0, &path, || true).is_err());
        assert!(
            store
                .set_file_size_limit(path.clone(), Some(size - 1))
                .unwrap()
        );
        assert!(scope(&store).read(0, &path, || true).is_err());
        assert!(store.set_file_size_limit(path.clone(), Some(size)).unwrap());
        let scope = scope(&store);
        let source = scope.read(0, &path, || true).unwrap();
        assert_eq!(source.bytes, bytes.len());
        assert_eq!(
            source
                .passages()
                .map(|(page, text)| (page, text.trim()))
                .collect::<Vec<_>>(),
            [
                (Location::Page(1), "Large PDF fixture text."),
                (Location::Page(3), "Last PDF fixture page."),
            ]
        );
        require_source_digests(&scope, &[Some(source.digest)], || Ok(())).unwrap();
        let mut oversized = bytes;
        oversized.push(b' ');
        fs::write(&path, oversized).unwrap();
        assert!(require_source_digests(&scope, &[Some(source.digest)], || Ok(())).is_err());
    }

    #[test]
    fn legacy_automatic_approvals_do_not_include_pdfs_until_reviewed() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("manual.pdf");
        std::fs::write(&path, crate::knowledge_pdf::fixture(&["Reviewed PDF text"])).unwrap();
        let mut explicit = Store::new("Explicit").unwrap();
        explicit.add_source(path.clone()).unwrap();
        explicit.automatic_files.insert(path.clone());
        assert_eq!(explicit.file_sync(&path), source::SyncMode::Manual);
        assert!(!explicit.has_automatic_sources());
        assert!(explicit.set_file_sync(path.clone(), source::SyncMode::Automatic));
        assert_eq!(explicit.file_sync(&path), source::SyncMode::Automatic);

        let folder = Store::new("Folder").unwrap();
        let (reply, _cancel) = folder.folder_review(profile.0.clone()).unwrap();
        let review = reply.recv_blocking().unwrap().unwrap();
        assert_eq!(review.folder.len(), 0);
        assert!(review.rows.iter().any(|row| matches!(
            row.status,
            folder::Status::Skipped("PDFs are outside the approved folder formats")
        )));
        let (reply, _cancel) = folder::start(
            profile.0.clone(),
            None,
            MAX_SOURCES,
            Vec::new(),
            source::Options {
                include_pdf: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reply.recv_blocking().unwrap().unwrap().folder.len(), 1);
    }

    #[test]
    fn knowledge_chunks_preserve_utf8_and_original_lines() {
        let row = "Ü".repeat(CHUNK_BYTES / 2 - 1);
        let input = format!("{row}\n{row}\nlast line");
        let passages: Vec<_> = chunks(&input).collect();
        assert_eq!(
            passages,
            [(1, row.as_str()), (2, row.as_str()), (3, "last line")]
        );
        let long_line = "界".repeat(CHUNK_BYTES);
        assert!(chunks(&long_line).all(|(line, text)| line == 1 && text.len() <= CHUNK_BYTES));
    }

    #[test]
    fn knowledge_ranking_validates_vector_dimensions_and_values() -> Result<(), String> {
        let mut query = [3.0, 4.0];
        normalize(&mut query, 2)?;
        let mut match_vector = [6.0, 8.0];
        normalize(&mut match_vector, 2)?;
        assert!((score(&query, &match_vector) - 1.0).abs() < f32::EPSILON);
        assert!(score(&query, &[-0.8, -0.6]) < 0.0);
        assert!(normalize(&mut [1.0], 2).is_err());
        assert!(normalize(&mut [f32::NAN, 1.0], 2).is_err());
        assert!(normalize(&mut [0.0, 0.0], 2).is_err());
        Ok(())
    }

    #[test]
    fn knowledge_folder_review_pins_contents_and_resolves_missing_sources() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.join("manual");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("chapters")).unwrap();
        fs::create_dir(root.join("vendor")).unwrap();
        for (path, contents) in [
            ("README", "Original introduction"),
            ("chapters/intro.MD", "One chapter"),
            ("vendor/secret.txt", "Excluded subtree"),
            (".hidden.txt", "Hidden notes"),
            ("invalid.txt", "NUL\0control"),
            ("archive.zip", "Unsupported format"),
            ("reversed\u{202e}name.txt", "Misleading display name"),
        ] {
            fs::write(root.join(path), contents).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("README"), root.join("link.txt")).unwrap();

        let mut store = Store::new("Manual").unwrap();
        let (reply, _cancel) = store.folder_review(root.clone()).unwrap();
        let initial = reply.recv_blocking().unwrap().unwrap();
        assert_eq!(initial.folder.len(), 2);
        assert!(initial.rows.iter().any(|row| {
            row.path == "vendor" && matches!(row.status, folder::Status::Skipped(_))
        }));
        assert!(initial.rows.iter().any(|row| {
            row.path.contains(r"\u{202e}") && matches!(row.status, folder::Status::Skipped(_))
        }));
        #[cfg(unix)]
        assert!(initial.rows.iter().any(|row| {
            row.path == "link.txt" && matches!(row.status, folder::Status::Skipped(_))
        }));
        assert_eq!(store.source_count(), 0);
        store.approve_folder(&initial).unwrap();
        assert!(store.add_source(root.join("README")).is_err());
        assert!(store.folder_review(root.join("chapters")).is_err());
        assert!(store.folder_review(profile.0.clone()).is_err());
        let restored: Store = serde_json::from_slice(&serde_json::to_vec(&store).unwrap()).unwrap();

        fs::write(root.join("README"), "Edited introduction").unwrap();
        fs::write(root.join("appendix.txt"), "Additional chapter").unwrap();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let scope = Scope::new(&restored, &endpoint, "embed").unwrap();
        assert_eq!(scope.sources.len(), 2);
        assert!(!scope.sources.contains(&root.join("appendix.txt")));
        let result = Index::build(&Client::new(scope.profile.clone()), &scope).unwrap_err();
        assert!(result.contains("reviewed folder file changed"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let (reply, _cancel) = store.folder_review(root.clone()).unwrap();
        let updated = reply.recv_blocking().unwrap().unwrap();
        assert!(updated.existing);
        assert_eq!(updated.folder.len(), 3);
        for expected in ["Changed", "New", "Unchanged"] {
            assert!(updated.rows.iter().any(|row| {
                matches!(row.status, folder::Status::Ready { change, .. } if change == expected)
            }));
        }
        assert_eq!(store.source_count(), 2);
        store.approve_folder(&updated).unwrap();
        assert_eq!(store.source_count(), 3);
        assert_ne!(
            scope.digest,
            Scope::new(&store, &endpoint, "embed").unwrap().digest
        );

        let moved = profile.0.join("moved");
        fs::rename(&root, &moved).unwrap();
        let (reply, _cancel) = store.folder_review(root).unwrap();
        let missing = reply.recv_blocking().unwrap().unwrap();
        assert!(missing.existing && missing.missing);
        assert_eq!(
            missing
                .rows
                .iter()
                .filter(|row| matches!(row.status, folder::Status::Removed))
                .count(),
            3
        );
        store.approve_folder(&missing).unwrap();
        assert_eq!(store.source_count(), 0);
        assert!(moved.join("appendix.txt").is_file());
    }

    #[test]
    fn knowledge_folder_review_caps_files_across_the_whole_store() {
        let profile = crate::snippets::tests::Profile::new();
        let mut store = Store::new("Manual").unwrap();
        for index in 1..MAX_SOURCES {
            store
                .add_source(profile.0.join(format!("manual-{index}.txt")))
                .unwrap();
        }
        let first = profile.0.join("first");
        let second = profile.0.join("second");
        for folder in [&first, &second] {
            fs::create_dir(folder).unwrap();
            fs::write(folder.join("README"), "Approved text").unwrap();
        }
        let (reply, _cancel) = store.folder_review(first).unwrap();
        store
            .approve_folder(&reply.recv_blocking().unwrap().unwrap())
            .unwrap();
        assert_eq!(store.source_count(), MAX_SOURCES);
        let (reply, _cancel) = store.folder_review(second).unwrap();
        assert!(
            reply
                .recv_blocking()
                .unwrap()
                .unwrap_err()
                .contains("store file limit")
        );
        assert_eq!(store.source_count(), MAX_SOURCES);
        assert_eq!(store.folders.len(), 1);
    }

    #[test]
    #[ignore = "requires the published signed llama.cpp package and a local GGUF fixture"]
    fn stable_channel_managed_embeddings_index_and_retrieve() {
        use crate::local_ai::{
            self, ProfileOptions,
            managed_models::{Purpose, Registry},
            runtime_package,
        };

        struct Stop<'a>(&'a Path, &'a str);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                let _ = runtime_package::stop_model(self.0, self.1);
            }
        }

        let profile = crate::snippets::tests::Profile::new();
        let asset = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
        let pending = runtime_package::start(
            profile.0.clone(),
            runtime_package::Import {
                index: asset("KORTTY_SMOKE_RUNTIME_INDEX"),
                signature: asset("KORTTY_SMOKE_RUNTIME_SIGNATURE"),
                archive: asset("KORTTY_SMOKE_RUNTIME_ZIP"),
                expected_backend: None,
            },
        )
        .unwrap();
        println!(
            "Activated {}",
            pending.reply.recv_blocking().unwrap().unwrap()
        );
        drop(pending);
        let registry = Registry::new(profile.0.clone());
        let model = registry
            .import(
                &asset("KORTTY_SMOKE_GGUF"),
                Purpose::Embedding,
                &Default::default(),
            )
            .unwrap();
        let _stop = Stop(&profile.0, &model.id);
        let test = runtime_package::test_model(profile.0.clone(), model.id.clone()).unwrap();
        assert_eq!(test.reply.recv_blocking().unwrap().unwrap(), model.name);
        drop(test);

        let source = profile.0.join("reference.txt");
        fs::write(&source, "The library returns borrowed books on Mondays.").unwrap();
        let mut store = Store::new("Reference").unwrap();
        store.add_source(source).unwrap();
        store.coding_profile = Some(ProfileRef::Numeric(7));
        let mut state = State::default();
        state.roles[super::super::Role::Coding as usize] = Some(ProfileRef::Numeric(7));
        state.managed_embedding = Some(model.id.clone());
        state.stores.push(store);
        let scope = state.scope(&state.stores[0], &profile.0).unwrap();
        let client = Client::new(scope.profile.clone());
        let index = Index::build(&client, &scope).unwrap();
        assert!(index.dimensions > 0);
        assert!(
            index
                .search(&client, &scope, "When are books returned?", || Ok(()))
                .unwrap()[0]
                .contains("Mondays")
        );
        drop(client);

        let cache = Cache::new(profile.0.clone());
        cache.save(&index, || Ok(())).unwrap();
        let reloaded: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let plan = reloaded
            .knowledge_for(
                super::super::Role::Coding,
                7,
                &profile.0,
                &profile_catalog(&[7]),
            )
            .pop()
            .unwrap();
        let pending = plan
            .start("When are books returned?".into(), 16_384, None)
            .unwrap();
        let retrieved = pending.reply.recv_blocking().unwrap().unwrap();
        assert_eq!(retrieved.citations()[0].file, "reference.txt");
        assert!(retrieved.context().contains("Mondays"));
        drop(pending);

        let wrong =
            local_ai::Profile::managed(profile.0.clone(), &model.id, ProfileOptions::default())
                .unwrap();
        assert!(
            Client::new(wrong)
                .complete("Test", "Test")
                .unwrap_err()
                .message()
                .contains("different purpose")
        );
        runtime_package::stop_model(&profile.0, &model.id).unwrap();
        println!(
            "Authenticated embeddings indexed a file and retrieved it from a reopened managed store"
        );
    }

    #[test]
    fn knowledge_index_survives_reopen_and_refuses_stale_sources() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("manual.txt");
        fs::write(&path, "Existing guide").unwrap();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            for input in ["Existing guide", "question"] {
                let mut stream = test_support::accept(&listener);
                let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap(),
                    json!({"model": "embed", "input": [input], "truncate": false})
                );
                let response =
                    test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1,0]]}"#);
                stream.get_mut().write_all(response.as_bytes()).unwrap();
            }
        });
        let mut store = Store::new("Manual").unwrap();
        store.add_source(path.clone()).unwrap();
        let state = State {
            endpoint,
            embedding: "embed".into(),
            stores: vec![store],
            ..State::default()
        };
        let scope = state.scope(&state.stores[0], &profile.0).unwrap();
        let client = Client::new(scope.profile.clone());
        let index = Arc::new(Index::build(&client, &scope).unwrap());
        let mut dialog = Dialog::new(profile.0.clone());
        dialog.drafts.push(super::super::DraftIndex {
            index: Arc::clone(&index),
            vault_revision: None,
        });

        let settings = serde_json::to_vec(&state).unwrap();
        let previous = State::default();
        let settings_path = profile.0.join("saved.json");
        let failing = dialog.save_indexes(&state, &previous).commit(
            || Err(io::Error::other("Could not save settings")),
            || Ok(()),
            None,
            true,
        );
        assert!(failing.is_err());
        assert_eq!(
            dialog.cache.load(&scope).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(
            dialog
                .save_indexes(&state, &previous)
                .commit(
                    || fs::write(&settings_path, settings),
                    || Ok(()),
                    None,
                    true
                )
                .unwrap()
                .is_none()
        );
        drop(dialog);

        let reopened: State = serde_json::from_slice(&fs::read(settings_path).unwrap()).unwrap();
        let scope = reopened.scope(&reopened.stores[0], &profile.0).unwrap();
        let cache = Cache::new(profile.0.clone());
        let loaded = cache.load(&scope).unwrap();
        assert!(
            loaded
                .search(&client, &scope, "question", || Ok(()))
                .unwrap()[0]
                .ends_with("Existing guide")
        );
        server.join().unwrap();

        let wrong_model = Scope::new(&reopened.stores[0], &reopened.endpoint, "different").unwrap();
        assert_eq!(
            cache.load(&wrong_model).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let mut candidate = cache.load(&scope).unwrap();
        candidate.entries[0].1 = vec![0.0, 1.0];
        let cancelled = cache.save(&candidate, || Err(io::ErrorKind::Interrupted.into()));
        assert_eq!(cancelled.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(cache.load(&scope).unwrap().entries[0].1, [1.0, 0.0]);

        fs::write(&path, "Different text").unwrap();
        assert!(
            loaded
                .search(&client, &scope, "question", || Ok(()))
                .unwrap_err()
                .contains("source changed")
        );
        cache.retain(&[]).unwrap();
        assert_eq!(
            cache.load(&scope).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "Different text");
    }

    #[test]
    fn knowledge_disabled_folder_retains_vectors_and_is_excluded_from_reopened_retrieval() {
        use super::super::Role;

        let profile = crate::snippets::tests::Profile::new();
        let manual = profile.0.join("manual.txt");
        let folder = profile.0.join("reference");
        let unavailable = profile.0.join("unavailable");
        fs::create_dir(&folder).unwrap();
        fs::write(&manual, "Enabled passage").unwrap();
        fs::write(folder.join("private.txt"), "Disabled passage").unwrap();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            for (input, vectors) in [
                (
                    json!(["Enabled passage", "Disabled passage"]),
                    json!([[1, 0], [0, 1]]),
                ),
                (json!(["Enabled passage"]), json!([[1, 0]])),
                (json!(["Enabled passage"]), json!([[1, 0, 0]])),
                (json!(["search visible"]), json!([[0, 1]])),
                (json!(["search restored"]), json!([[0, 1]])),
            ] {
                let mut stream = test_support::accept(&listener);
                let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap()["input"],
                    input
                );
                let response = test_support::fixture_http_response(
                    "200 OK",
                    "",
                    &json!({"embeddings": vectors}).to_string(),
                );
                stream.get_mut().write_all(response.as_bytes()).unwrap();
            }
        });
        let mut store = Store::new("Reference").unwrap();
        store.add_source(manual).unwrap();
        let (review, _cancel) = store.folder_review(folder.clone()).unwrap();
        store
            .approve_folder(&review.recv_blocking().unwrap().unwrap())
            .unwrap();
        store.text_profile = Some(ProfileRef::Numeric(7));
        let mut state = State {
            endpoint,
            embedding: "embed".into(),
            stores: vec![store],
            ..State::default()
        };
        state.roles[Role::Text as usize] = Some(ProfileRef::Numeric(7));
        let original = state.clone();
        let scope = state.scope(&state.stores[0], &profile.0).unwrap();
        let client = Client::new(scope.profile.clone());
        let cache = Cache::new(profile.0.clone());
        let index = Index::build(&client, &scope).unwrap();
        cache.save(&index, || Ok(())).unwrap();
        let (_, stamp) = cache.load_stamped(&scope).unwrap();

        let mut dialog = Dialog::new(profile.0.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut state, &original, true);
        let _ = dialog.update(
            Message::SourceEnabled {
                store: 0,
                kind: SourceKind::Folder,
                path: folder.clone(),
                enabled: false,
            },
            &mut state,
            &original,
            true,
        );
        assert!(!state.stores[0].source_enabled(SourceKind::Folder, &folder));
        fs::rename(&folder, &unavailable).unwrap();
        let settings = serde_json::to_vec(&state).unwrap();
        let settings_path = profile.0.join("saved.json");
        dialog
            .save_indexes(&state, &original)
            .commit(
                || fs::write(&settings_path, settings),
                || Ok(()),
                None,
                true,
            )
            .unwrap();
        let mut reopened: State =
            serde_json::from_slice(&fs::read(settings_path).unwrap()).unwrap();
        let filtered = reopened.scope(&reopened.stores[0], &profile.0).unwrap();
        assert!(scope.same_snapshot(&filtered));
        cache.require_current(&filtered, &stamp).unwrap();
        let (updated, summary) =
            Index::update(&client, &filtered, Some((&scope, &index)), false, || Ok(())).unwrap();
        assert_eq!(
            summary,
            UpdateSummary {
                reused: 1,
                indexed: 0,
                removed: 0
            }
        );
        assert_eq!(updated.len(), 2);
        let (rebuilt, _) =
            Index::update(&client, &filtered, Some((&scope, &index)), true, || Ok(())).unwrap();
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(rebuilt.entries[1].1, [0.0, 1.0]);
        let (incompatible, _) =
            Index::update(&client, &filtered, Some((&scope, &index)), true, || Ok(())).unwrap();
        assert_eq!(incompatible.len(), 1);
        assert_eq!(incompatible.sources[1], None);
        let plan = reopened
            .knowledge_for(Role::Text, 7, &profile.0, &profile_catalog(&[7]))
            .pop()
            .unwrap();
        let pending = plan.start("search visible".into(), 16_384, None).unwrap();
        let result = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(result.context().contains("Enabled passage"));
        assert!(!result.context().contains("Disabled passage"));
        drop(pending);

        assert!(reopened.stores[0].set_source_enabled(SourceKind::Folder, folder.clone(), true));
        let restored = reopened.scope(&reopened.stores[0], &profile.0).unwrap();
        assert!(index.check_sources(&restored, || Ok(())).is_err());
        fs::rename(unavailable, &folder).unwrap();
        assert!(
            cache
                .load(&restored)
                .unwrap()
                .search(&client, &restored, "search restored", || Ok(()))
                .unwrap()[0]
                .contains("Disabled passage")
        );
        server.join().unwrap();
    }

    #[test]
    fn knowledge_disabled_unindexed_file_round_trips_without_reading_or_embedding_it() {
        let profile = crate::snippets::tests::Profile::new();
        let active = profile.0.join("active.txt");
        let inactive = profile.0.join("inactive.txt");
        fs::write(&active, "Active excerpt").unwrap();
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            for input in ["Active excerpt", "Newly enabled excerpt"] {
                let mut stream = test_support::accept(&listener);
                let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap()["input"],
                    json!([input])
                );
                let response =
                    test_support::fixture_http_response("200 OK", "", r#"{"embeddings":[[1,0]]}"#);
                stream.get_mut().write_all(response.as_bytes()).unwrap();
            }
        });
        let mut store = Store::new("Reference").unwrap();
        store.add_source(active).unwrap();
        store.add_source(inactive.clone()).unwrap();
        assert!(store.set_source_enabled(SourceKind::File, inactive.clone(), false));
        let scope = Scope::new(&store, &endpoint, "embed").unwrap();
        let client = Client::new(scope.profile.clone());
        let cache = Cache::new(profile.0.clone());
        cache
            .save(&Index::build(&client, &scope).unwrap(), || Ok(()))
            .unwrap();
        let cached = cache.load(&scope).unwrap();
        assert_eq!(cached.sources[1], None);
        assert_eq!(cached.len(), 1);

        assert!(store.set_source_enabled(SourceKind::File, inactive.clone(), true));
        let enabled = Scope::new(&store, &endpoint, "embed").unwrap();
        assert!(
            cached
                .check_sources(&enabled, || Ok(()))
                .unwrap_err()
                .contains("missing from the index")
        );
        fs::write(inactive, "Newly enabled excerpt").unwrap();
        let (updated, summary) =
            Index::update(&client, &enabled, Some((&scope, &cached)), false, || Ok(())).unwrap();
        assert_eq!(
            summary,
            UpdateSummary {
                reused: 1,
                indexed: 1,
                removed: 0
            }
        );
        assert!(updated.sources.iter().all(Option::is_some));
        server.join().unwrap();
    }

    #[test]
    fn knowledge_update_reuses_unchanged_folder_vectors_across_a_saved_scope_change() {
        use crate::ai_manager::Configuration;
        use iced::futures::{StreamExt as _, executor::block_on};

        let _exclusive = test_support::exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.join("docs");
        fs::create_dir(&root).unwrap();
        for (file, text) in [
            ("a-keep.txt", "Unchanged reference"),
            ("b-edit.txt", "Previous excerpt"),
            ("c-remove.txt", "Removed excerpt"),
        ] {
            fs::write(root.join(file), text).unwrap();
        }
        let (listener, _) = test_support::listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            for (input, vectors) in [
                (
                    json!(["Unchanged reference", "Previous excerpt", "Removed excerpt"]),
                    json!([[1, 0], [1, 1], [0, 1]]),
                ),
                (
                    json!(["Updated excerpt", "New reference"]),
                    json!([[0, 0, 1], [1, 0, 0]]),
                ),
                (
                    json!(["Updated excerpt", "New reference"]),
                    json!([[0, 1], [1, 1]]),
                ),
                (
                    json!(["Unchanged reference", "Updated excerpt", "New reference"]),
                    json!([[0, 0, 1], [1, 0, 0], [0, 1, 0]]),
                ),
            ] {
                let mut stream = test_support::accept(&listener);
                let body = test_support::wire_request(&mut stream, "POST /api/embed HTTP/1.1");
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap(),
                    json!({
                        "model": "embed", "input": input, "truncate": false
                    })
                );
                let response = test_support::fixture_http_response(
                    "200 OK",
                    "",
                    &json!({"embeddings": vectors}).to_string(),
                );
                stream.get_mut().write_all(response.as_bytes()).unwrap();
            }
        });

        let mut store = Store::new("Manual").unwrap();
        let (review, _cancel) = store.folder_review(root.clone()).unwrap();
        store
            .approve_folder(&review.recv_blocking().unwrap().unwrap())
            .unwrap();
        let mut saved = State {
            endpoint,
            embedding: "embed".into(),
            stores: vec![store],
            ..State::default()
        };
        let configuration = Configuration {
            local: saved.clone(),
            ..Configuration::default()
        };
        configuration
            .save_against(&profile.0, &Configuration::default())
            .unwrap();
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let finish = |dialog: &mut Dialog, draft: &mut State, saved: &State, message| {
            let task = dialog.update(message, draft, saved, true);
            let mut stream =
                iced_runtime::task::into_stream(task).expect("scheduled local AI step");
            let Some(iced_runtime::Action::Output(reply)) = block_on(stream.next()) else {
                panic!("local AI step did not return a result");
            };
            let _ = dialog.update(reply, draft, saved, true);
        };
        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);
        let _ = dialog.update(Message::Consent(true), &mut draft, &saved, true);
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::Index { rebuild: false },
        );
        let old_scope = saved.scope(&saved.stores[0], &profile.0).unwrap();
        assert_eq!(
            dialog.cache.load(&old_scope).unwrap().entries[0].1,
            [1.0, 0.0]
        );

        fs::write(root.join("b-edit.txt"), "Updated excerpt").unwrap();
        fs::remove_file(root.join("c-remove.txt")).unwrap();
        fs::write(root.join("d-add.txt"), "New reference").unwrap();
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::ReviewFolderUpdate(0),
        );
        let _ = dialog.update(Message::ApproveFolder, &mut draft, &saved, true);
        let without_consent =
            dialog.update(Message::Index { rebuild: false }, &mut draft, &saved, true);
        assert!(iced_runtime::task::into_stream(without_consent).is_none());
        let _ = dialog.update(Message::Consent(true), &mut draft, &saved, true);
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::Index { rebuild: false },
        );
        assert!(
            dialog
                .status
                .as_deref()
                .unwrap()
                .contains("dimensions changed")
        );
        assert!(dialog.drafts.is_empty());
        assert!(dialog.cache.load(&old_scope).is_ok());
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::Index { rebuild: false },
        );
        assert!(
            dialog
                .status
                .as_deref()
                .unwrap()
                .contains("files reused 1, indexed 2, removed 1")
        );
        let new_scope = draft.scope(&draft.stores[0], &profile.0).unwrap();
        assert_eq!(
            dialog.cache.load(&new_scope).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(dialog.cache.load(&old_scope).is_ok());

        let next = Configuration {
            local: draft.clone(),
            ..Configuration::default()
        };
        let save = || {
            next.save_with_indexes(
                &profile.0,
                &configuration,
                Some(dialog.save_indexes(&draft, &saved)),
                None,
            )
        };
        assert!(save().unwrap().is_none());
        let (_, committed_stamp) = dialog.cache.load_stamped(&new_scope).unwrap();
        assert!(
            save()
                .unwrap_err()
                .to_string()
                .contains("AI settings changed")
        );
        dialog
            .cache
            .require_current(&new_scope, &committed_stamp)
            .unwrap();
        assert_eq!(
            dialog.cache.load(&old_scope).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let committed = dialog.cache.load(&new_scope).unwrap();
        assert_eq!(committed.entries[0].1, [1.0, 0.0]);
        assert_eq!(committed.entries[1].1, [0.0, 1.0]);
        assert_eq!(committed.entries[2].0.text, "New reference");
        let (_, stamp) = dialog.cache.load_stamped(&new_scope).unwrap();
        saved = draft.clone();
        dialog = Dialog::new(profile.0.clone());
        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, true);
        let _ = dialog.update(Message::Consent(true), &mut draft, &saved, true);
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::Index { rebuild: false },
        );
        assert!(
            dialog
                .status
                .as_deref()
                .unwrap()
                .contains("files reused 3, indexed 0")
        );
        dialog.cache.require_current(&new_scope, &stamp).unwrap();
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::Index { rebuild: true },
        );
        assert_eq!(
            dialog.cache.load(&new_scope).unwrap().entries[0].1,
            [0.0, 0.0, 1.0]
        );
        server.join().unwrap();

        let snapshot = fs::read_dir(profile.0.join("knowledge-indexes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|extension| extension == "rag"))
            .unwrap();
        fs::write(snapshot, b"invalid index").unwrap();
        finish(
            &mut dialog,
            &mut draft,
            &saved,
            Message::Index { rebuild: false },
        );
        assert!(
            dialog
                .status
                .as_deref()
                .unwrap()
                .contains("Choose Rebuild index")
        );
    }
}
