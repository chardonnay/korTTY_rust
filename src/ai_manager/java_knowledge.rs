//! Portable, inactive definitions from the Java knowledge-store registry.

use std::collections::{HashMap, HashSet};
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use url::Url;

use super::invalid;

pub(crate) const MEMBER: &str = "rag/stores.json";
const MAX_REGISTRY_BYTES: usize = 1024 * 1024;
const MAX_STORES: usize = 64;
const MAX_SOURCES: usize = 256;
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_FILE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum Backend {
    LocalHnsw,
    Qdrant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum Kind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum Sync {
    Manual,
    Automatic,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Origin {
    pub(super) id: String,
    definition: [u8; 32],
}

impl Origin {
    pub(super) fn valid(&self) -> bool {
        valid_id(&self.id) && self.definition != [0; 32]
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Store {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) backend: Backend,
    pub(super) qdrant_origin: Option<String>,
    pub(super) collection: Option<String>,
    pub(super) embedding_model_id: Option<String>,
    pub(super) embedding_dimensions: u32,
    pub(super) text_enabled: bool,
    pub(super) coding_enabled: bool,
    pub(super) autonomous_enabled: bool,
    pub(super) sources: Vec<Source>,
}

impl Store {
    pub(super) fn origin(&self) -> io::Result<Origin> {
        let encoded = serde_json::to_vec(self).map_err(io::Error::other)?;
        Ok(Origin {
            id: self.id.clone(),
            definition: Sha256::digest(encoded).into(),
        })
    }

    fn valid(&self) -> bool {
        let mut seen = HashSet::new();
        valid_id(&self.id)
            && bounded(&self.name, 512)
            && self.embedding_model_id.as_deref().is_none_or(valid_id)
            && (self.embedding_model_id.is_none() || self.embedding_dimensions > 0)
            && self.embedding_dimensions <= i32::MAX as u32
            && (self.backend == Backend::Qdrant
                || self.qdrant_origin.is_none() && self.collection.is_none())
            && self
                .qdrant_origin
                .as_deref()
                .is_none_or(|value| endpoint_origin(value).as_deref() == Some(value))
            && self.collection.as_deref().is_none_or(valid_collection)
            && self.sources.len() <= MAX_SOURCES
            && self
                .sources
                .iter()
                .all(|source| source.valid() && seen.insert(&source.id))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Source {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) basename: Option<String>,
    pub(super) kind: Kind,
    pub(super) sync: Sync,
    pub(super) enabled: bool,
    pub(super) includes: Vec<String>,
    pub(super) excludes: Vec<String>,
    pub(super) recursive: bool,
    pub(super) respect_gitignore: bool,
    pub(super) max_file_bytes: u64,
}

impl Source {
    fn valid(&self) -> bool {
        let patterns = |values: &[String]| {
            values.len() <= 64 && values.iter().all(|value| bounded(value, 512))
        };
        valid_id(&self.id)
            && bounded(&self.name, 512)
            && self.basename.as_deref().is_none_or(valid_basename)
            && (!self.recursive || self.kind == Kind::Directory)
            && patterns(&self.includes)
            && patterns(&self.excludes)
            && (1..=MAX_FILE_BYTES).contains(&self.max_file_bytes)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(transparent)]
pub(super) struct Hints(Vec<Store>);

impl Hints {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Store> {
        self.0.iter()
    }

    pub(super) fn get(&self, id: &str) -> Option<&Store> {
        self.iter().find(|store| store.id == id)
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        let mut seen = HashSet::new();
        if self.0.len() > MAX_STORES
            || !self
                .iter()
                .all(|store| store.valid() && seen.insert(&store.id))
        {
            return Err(invalid("Invalid Java knowledge store suggestions"));
        }
        Ok(())
    }

    pub(super) fn merge(&mut self, incoming: Self) -> usize {
        let existing = self.iter().map(|store| &store.id).collect::<HashSet<_>>();
        let additions = incoming
            .0
            .into_iter()
            .filter(|store| !existing.contains(&store.id))
            .take(MAX_STORES - self.0.len())
            .collect::<Vec<_>>();
        let added = additions.len();
        self.0.extend(additions);
        added
    }
}

pub(super) struct Import {
    pub(super) hints: Hints,
    pub(super) stores: usize,
    pub(super) sources: usize,
    pub(super) stores_omitted: usize,
    pub(super) sources_omitted: usize,
    pub(super) destinations_omitted: usize,
}

#[derive(Deserialize)]
struct Registry<'a> {
    version: u32,
    #[serde(borrow)]
    stores: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct Entry<'a> {
    #[serde(borrow)]
    store: &'a RawValue,
    #[serde(borrow)]
    sources: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct Identity<'a> {
    #[serde(borrow)]
    id: std::borrow::Cow<'a, str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchivedStore {
    id: String,
    display_name: String,
    #[serde(rename = "type")]
    backend: Backend,
    endpoint: Option<String>,
    collection_name: Option<String>,
    embedding_model_id: Option<String>,
    embedding_dimensions: Option<u32>,
    text_enabled: Option<bool>,
    coding_enabled: Option<bool>,
    autonomous_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchivedSource {
    id: String,
    display_name: String,
    path: String,
    #[serde(rename = "type")]
    kind: Kind,
    sync_mode: Sync,
    enabled: bool,
    include_patterns: Vec<String>,
    exclude_patterns: Vec<String>,
    recursive: Option<bool>,
    respect_git_ignore: Option<bool>,
    max_file_bytes: Option<u64>,
}

pub(super) fn import(bytes: &[u8]) -> io::Result<Import> {
    if bytes.len() > MAX_REGISTRY_BYTES {
        return Err(invalid(
            "Java knowledge store registry exceeds its size limit",
        ));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let registry: Registry<'_> = serde_json::from_slice(bytes)
        .map_err(|_| invalid("Invalid Java knowledge store registry JSON"))?;
    if registry.version != 1 {
        return Err(invalid("Unsupported Java knowledge store registry schema"));
    }
    let total_stores = registry.stores.len();
    let mut candidates = Vec::new();
    let mut identities = HashMap::new();
    for raw in registry.stores {
        // Count a store's ID even when another member of its entry is invalid.
        #[derive(Deserialize)]
        struct StoreOnly<'a> {
            #[serde(borrow)]
            store: &'a RawValue,
        }
        if let Ok(candidate) = serde_json::from_str::<StoreOnly<'_>>(raw.get()) {
            claim(&mut identities, candidate.store);
        }
        if let Ok(entry) = serde_json::from_str::<Entry<'_>>(raw.get()) {
            candidates.push(entry);
        }
    }
    let mut report = Import {
        hints: Hints::default(),
        stores: 0,
        sources: 0,
        stores_omitted: 0,
        sources_omitted: 0,
        destinations_omitted: 0,
    };
    for entry in candidates {
        if report.hints.0.len() == MAX_STORES {
            break;
        }
        let Ok(archived) = serde_json::from_str::<ArchivedStore>(entry.store.get()) else {
            continue;
        };
        let mut store = archived.project(&mut report.destinations_omitted);
        if identities.get(&store.id) != Some(&1) || !store.valid() {
            continue;
        }
        store.sources = project_sources(entry.sources, &mut report.sources_omitted);
        report.sources += store.sources.len();
        report.hints.0.push(store);
    }
    report.stores = report.hints.0.len();
    report.stores_omitted = total_stores - report.stores;
    Ok(report)
}

impl ArchivedStore {
    fn project(self, destinations_omitted: &mut usize) -> Store {
        let (qdrant_origin, collection) = if self.backend == Backend::Qdrant {
            let origin = self.endpoint.as_deref().and_then(endpoint_origin);
            let collection = self.collection_name.filter(|value| valid_collection(value));
            *destinations_omitted +=
                usize::from(origin.is_none()) + usize::from(collection.is_none());
            (origin, collection)
        } else {
            (None, None)
        };
        Store {
            id: self.id,
            name: self.display_name.trim().into(),
            backend: self.backend,
            qdrant_origin,
            collection,
            embedding_model_id: self.embedding_model_id.filter(|id| !id.is_empty()),
            embedding_dimensions: self.embedding_dimensions.unwrap_or(0),
            text_enabled: self.text_enabled.unwrap_or(true),
            coding_enabled: self.coding_enabled.unwrap_or(true),
            autonomous_enabled: self.autonomous_enabled.unwrap_or(false),
            sources: Vec::new(),
        }
    }
}

fn project_sources(raw: Vec<&RawValue>, omitted: &mut usize) -> Vec<Source> {
    let total = raw.len();
    let mut identities = HashMap::new();
    raw.iter().for_each(|value| claim(&mut identities, value));
    let sources = raw
        .into_iter()
        .filter_map(|raw| serde_json::from_str::<ArchivedSource>(raw.get()).ok())
        .map(|source| Source {
            id: source.id,
            name: source.display_name.trim().into(),
            basename: basename(&source.path),
            kind: source.kind,
            sync: source.sync_mode,
            enabled: source.enabled,
            includes: source.include_patterns,
            excludes: source.exclude_patterns,
            recursive: source.kind == Kind::Directory && source.recursive.unwrap_or(true),
            respect_gitignore: source.respect_git_ignore.unwrap_or(true),
            max_file_bytes: source.max_file_bytes.unwrap_or(DEFAULT_FILE_BYTES),
        })
        .filter(|source| identities.get(&source.id) == Some(&1) && source.valid())
        .take(MAX_SOURCES)
        .collect::<Vec<_>>();
    *omitted += total - sources.len();
    sources
}

fn claim(keys: &mut HashMap<String, usize>, value: &RawValue) {
    if let Ok(identity) = serde_json::from_str::<Identity<'_>>(value.get()) {
        let id = identity.id.trim();
        if valid_id(id) {
            *keys.entry(id.into()).or_default() += 1;
        }
    }
}

fn bounded(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_id(value: &str) -> bool {
    bounded(value, 128)
}

fn valid_basename(value: &str) -> bool {
    bounded(value, 256) && !matches!(value, "." | "..") && !value.contains(['/', '\\', ':'])
}

fn basename(path: &str) -> Option<String> {
    let value = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()?;
    (value != path && valid_basename(value)).then(|| value.to_owned())
}

fn valid_collection(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn endpoint_origin(value: &str) -> Option<String> {
    let (_, target) = value.split_once("://")?;
    let authority = target.split(['/', '?', '#']).next()?;
    if !bounded(value, 2048) || value.contains(['\\', '?', '#']) || authority.contains('@') {
        return None;
    }
    let url = Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}

#[cfg(test)]
pub(crate) const FIXTURE: &[u8] = br#"{
  "version": 1,
  "stores": [
    {"store":{"id":"local-store","displayName":"Project notes","type":"LOCAL_HNSW","localDirectory":"/archive/rag-private-index","collectionName":"kortty_rag","apiKey":"","embeddingModelId":"embed-model","embeddingDimensions":768,"textEnabled":true,"codingEnabled":false,"autonomousEnabled":true},
     "sources":[{"id":"tree","displayName":"Library","path":"C:\\archive\\private\\source-tree","type":"DIRECTORY","syncMode":"AUTOMATIC","enabled":true,"includePatterns":["**/*.md"],"excludePatterns":["draft/**"],"recursive":false,"respectGitIgnore":true,"maxFileBytes":1048576,"lastStatus":"READY","documentHashes":{"/archive/foreign-document":"foreign-digest"},"indexedFiles":32,"indexedChunks":86}]},
    {"store":{"id":"remote-store","displayName":"Java Qdrant","type":"QDRANT","endpoint":"https://rag.example:6334/path-to-omit","collectionName":"java_collection","apiKey":"omitted-api-key","embeddingModelId":"","embeddingDimensions":0},
     "sources":[{"id":"tree","displayName":"Reference","path":"/archive/private/source.txt","type":"FILE","syncMode":"MANUAL","enabled":false,"includePatterns":[],"excludePatterns":[]}]}
  ]
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_knowledge_suggestions_drop_ambiguous_store_and_source_claims() {
        let source = r#"{"id":"same-source","displayName":"One","path":"/hidden/a","type":"FILE","syncMode":"MANUAL","enabled":false,"includePatterns":[],"excludePatterns":[]}"#;
        let registry = format!(
            r#"{{"version":1,"stores":[
            {{"store":{{"id":"duplicate","displayName":"First","type":"LOCAL_HNSW"}},"sources":[]}},
            {{"store":{{"id":" duplicate "}},"sources":false}},
            {{"store":{{"id":"retained","displayName":"Third","type":"QDRANT","endpoint":"https://user:secret@host.example/api?token=hidden","collectionName":".."}},
              "sources":[{source},{{"id":" same-source ","displayName":false}}]}}
        ]}}"#
        );
        let report = import(registry.as_bytes()).unwrap();
        assert_eq!(report.stores, 1);
        assert_eq!(report.stores_omitted, 2);
        assert_eq!(report.sources_omitted, 2);
        assert_eq!(report.destinations_omitted, 2);
        let store = report.hints.get("retained").unwrap();
        assert!(store.sources.is_empty());
        assert!(store.qdrant_origin.is_none() && store.collection.is_none());
        assert!(import(br#"{"version":2,"stores":[]}"#).is_err());
    }
}
