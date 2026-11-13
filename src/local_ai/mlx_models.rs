//! Private, data-only registrations for the managed Apple MLX sidecar.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::private_files::{Directory, EntryIdentity, FileStamp, nonce, require_single_link};

use super::managed_models::valid_id;

pub(crate) const BACKUP_DIRECTORY: &str = "mlx-models";
const ROOT: &str = BACKUP_DIRECTORY;
const STAGING: &str = "staging";
pub(crate) const BACKUP_MANIFEST: &str = "model.json";
const MANIFEST: &str = BACKUP_MANIFEST;
const LOCK: &str = "registry.lock";
const LEASE: &str = "model.lock";
const INDEX: &str = "model.safetensors.index.json";
pub(crate) const MAX_MODELS: usize = 128;
const MAX_FILES: usize = 128;
const MAX_BYTES: u64 = 128 * 1024 * 1024 * 1024;
const MAX_METADATA: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_MANIFEST: u64 = 64 * 1024;
pub(crate) const DEFAULT_IDLE_MINUTES: u16 = 10;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Model {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) bytes: u64,
    pub(crate) idle_minutes: u16,
    assets: BTreeMap<String, u64>,
}

pub(crate) struct Review {
    source: PathBuf,
    identity: EntryIdentity,
    stamps: BTreeMap<String, FileStamp>,
    model: Model,
}

impl Review {
    pub(crate) fn prepare(path: &Path, cancelled: &AtomicBool) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid("Choose an absolute local MLX model directory"));
        }
        let source = path.canonicalize()?;
        let name = source
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid("Choose a named MLX model directory"))?;
        require_name(name)?;
        let directory = Directory::open(&source, false)?;
        let identity = directory.identity()?;
        let mut files = BTreeMap::new();
        for name in directory.names(MAX_FILES)? {
            check_cancelled(cancelled)?;
            let Some(name) = name.to_str().filter(|name| is_asset(name)) else {
                continue;
            };
            let file = directory.open_file(name)?;
            require_single_link(&file)?;
            files.insert(name.to_owned(), file);
        }
        let assets = files
            .iter()
            .map(|(name, file)| Ok((name.clone(), file.metadata()?.len())))
            .collect::<io::Result<_>>()?;
        let model = Model {
            id: nonce()?,
            name: name.to_owned(),
            bytes: size_of_assets(&assets)?,
            idle_minutes: DEFAULT_IDLE_MINUTES,
            assets,
        };
        let stamps = stamps(&files)?;
        validate_assets(&model, &mut files)?;
        for (name, file) in &files {
            stamps[name].require_unchanged(file)?;
        }
        check_cancelled(cancelled)?;
        Ok(Self {
            source,
            identity,
            stamps,
            model,
        })
    }

    pub(crate) fn model(&self) -> &Model {
        &self.model
    }

    pub(crate) fn source(&self) -> &Path {
        &self.source
    }

    pub(crate) fn asset_names(&self) -> impl Iterator<Item = &str> {
        self.model.assets.keys().map(String::as_str)
    }
}

pub(crate) struct Registry<'a>(&'a Path);

impl<'a> Registry<'a> {
    pub(crate) fn new(profile: &'a Path) -> Self {
        Self(profile)
    }

    pub(crate) fn model_name(&self, id: &str) -> io::Result<String> {
        require_id(id)?;
        let root = self.root(false)?;
        let _lock = lock(&root, LOCK, true)?;
        read_model(&root.private_child(id, false)?, id).map(|model| model.name)
    }

    pub(crate) fn list(&self) -> io::Result<Vec<Model>> {
        let root = match self.root(false) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let _lock = lock(&root, LOCK, true)?;
        let mut models = ids(&root)?
            .into_iter()
            .map(|id| read_model(&root.private_child(&id, false)?, &id))
            .collect::<io::Result<Vec<_>>>()?;
        models.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Ok(models)
    }

    pub(crate) fn import(&self, review: &Review, cancelled: &AtomicBool) -> io::Result<Model> {
        let source = Directory::open(&review.source, false)?;
        if source.identity()? != review.identity {
            return Err(invalid("The reviewed MLX model directory changed"));
        }
        let root = self.root(true)?;
        let _lock = lock(&root, LOCK, false)?;
        let staging = prepare_staging(&root)?;
        let model = &review.model;
        if ids(&root)?.len() >= MAX_MODELS {
            return Err(invalid("The MLX model registry is full"));
        }
        let stage = staging.create_private_subdirectory(OsStr::new(&model.id))?;
        let prepared = (|| {
            let mut buffer = vec![0; 128 * 1024];
            for (name, bytes) in &model.assets {
                let mut file = source.open_file(name)?;
                require_single_link(&file)?;
                let stamp = &review.stamps[name];
                stamp.require_unchanged(&file)?;
                stage.publish_with(name, |output| {
                    copy(&mut file, output, *bytes, &mut buffer, cancelled)?;
                    stamp.require_unchanged(&file)
                })?;
            }
            // Check the complete reviewed set again before publishing the private copy.
            for (name, stamp) in &review.stamps {
                stamp.require_unchanged(&source.open_file(name)?)?;
            }
            stage.publish_copy(
                MANIFEST,
                &serde_json::to_vec(model).map_err(io::Error::other)?,
            )?;
            stage.sync()?;
            check_cancelled(cancelled)
        })();
        drop(stage);
        if let Err(error) = prepared.and_then(|()| staging.move_subdirectory(&model.id, &root)) {
            discard_stage(&staging, &model.id)?;
            return Err(error);
        }
        root.sync()?;
        staging.sync()?;
        Ok(model.clone())
    }

    pub(crate) fn open(&self, id: &str) -> io::Result<Opened> {
        require_id(id)?;
        let root = self.root(false)?;
        let _lock = lock(&root, LOCK, true)?;
        let directory = root.private_child(id, false)?;
        let lease = lock(&directory, LEASE, true)?;
        let model = read_model(&directory, id)?;
        let mut files = BTreeMap::new();
        for (name, bytes) in &model.assets {
            let file = directory.open_private_file(name)?;
            require_single_link(&file)?;
            if file.metadata()?.len() != *bytes {
                return Err(invalid("An installed MLX model file changed size"));
            }
            files.insert(name.clone(), file);
        }
        let stamps = stamps(&files)?;
        validate_assets(&model, &mut files)?;
        let opened = Opened {
            model,
            path: self.0.join(ROOT).join(id),
            directory,
            files,
            stamps,
            _lease: Some(lease),
        };
        opened.revalidate()?;
        Ok(opened)
    }

    pub(crate) fn configure(&self, id: &str, name: &str, idle_minutes: u16) -> io::Result<Model> {
        require_id(id)?;
        let name = name.trim();
        require_name(name)?;
        if idle_minutes > 1_440 {
            return Err(invalid("Set the MLX idle timeout from 0 to 1440 minutes"));
        }
        let root = self.root(false)?;
        let _lock = lock(&root, LOCK, false)?;
        let directory = root.private_child(id, false)?;
        let _lease = lock(&directory, LEASE, false)?;
        let mut model = read_model(&directory, id)?;
        model.name = name.to_owned();
        model.idle_minutes = idle_minutes;
        directory.replace_file(
            MANIFEST,
            &serde_json::to_vec(&model).map_err(io::Error::other)?,
        )?;
        directory.sync()?;
        Ok(model)
    }

    pub(crate) fn remove(&self, id: &str) -> io::Result<()> {
        require_id(id)?;
        let root = self.root(false)?;
        let _lock = lock(&root, LOCK, false)?;
        let directory = root.private_child(id, false)?;
        let lease = lock(&directory, LEASE, false)?;
        let staging = prepare_staging(&root)?;
        drop(lease);
        drop(directory);
        root.move_subdirectory(id, &staging)?;
        root.sync()?;
        staging.sync()?;
        discard_stage(&staging, id)
    }

    fn root(&self, create: bool) -> io::Result<Directory> {
        Directory::open(self.0, create)?.private_child(ROOT, create)
    }
}

pub(crate) struct Opened {
    pub(crate) model: Model,
    pub(crate) path: PathBuf,
    directory: Directory,
    files: BTreeMap<String, File>,
    stamps: BTreeMap<String, FileStamp>,
    _lease: Option<File>,
}

impl Opened {
    pub(crate) fn revalidate(&self) -> io::Result<()> {
        #[cfg(unix)]
        if self._lease.is_none() {
            let (directory, files) = open_policy_directory(&self.path, &AtomicBool::new(false))?;
            if !self.directory.same_as(&directory)? || files.len() != self.files.len() {
                return Err(invalid("The policy MLX model directory changed"));
            }
            for (name, file) in &self.files {
                let current = files
                    .get(name)
                    .ok_or_else(|| invalid("The policy MLX model inventory changed"))?;
                self.stamps[name].require_unchanged(file)?;
                self.stamps[name].require_unchanged(current)?;
            }
            return Ok(());
        }
        #[cfg(unix)]
        let reopened = Directory::open_owner_controlled(&self.path)?;
        #[cfg(windows)]
        let reopened = Directory::open(&self.path, false)?;
        if !self.directory.same_as(&reopened)? {
            return Err(invalid("The installed MLX model directory changed"));
        }
        let names = self.directory.names(MAX_FILES + 2)?;
        if names.len() != self.files.len() + 2
            || names.iter().any(|name| {
                !name.to_str().is_some_and(|name| {
                    matches!(name, MANIFEST | LEASE) || self.files.contains_key(name)
                })
            })
        {
            return Err(invalid("The installed MLX model inventory changed"));
        }
        for (name, file) in &self.files {
            let current = reopened.open_private_file(name)?;
            require_single_link(&current)?;
            let stamp = &self.stamps[name];
            stamp.require_unchanged(file)?;
            stamp.require_unchanged(&current)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(super) fn policy_identity(
        &self,
    ) -> io::Result<(EntryIdentity, &BTreeMap<String, FileStamp>)> {
        Ok((self.directory.identity()?, &self.stamps))
    }
}

#[cfg(unix)]
fn open_policy_directory(
    path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<(Directory, BTreeMap<String, File>)> {
    let directory = Directory::open_administrator_controlled(path)?;
    let mut files = BTreeMap::new();
    for name in directory.names(MAX_FILES)? {
        check_cancelled(cancelled)?;
        let name = name.to_str().filter(|name| is_asset(name)).ok_or_else(|| {
            invalid("A policy MLX directory may contain only supported model assets")
        })?;
        let file = directory.open_file(name)?;
        require_single_link(&file)?;
        crate::private_files::require_administrator_controlled(&file)?;
        super::managed_models::require_local_source(&file)?;
        files.insert(name.to_owned(), file);
    }
    Ok((directory, files))
}

#[cfg(unix)]
pub(super) fn policy_source_identity(
    path: &Path,
) -> io::Result<(EntryIdentity, BTreeMap<String, FileStamp>)> {
    let (directory, files) = open_policy_directory(path, &AtomicBool::new(false))?;
    Ok((directory.identity()?, stamps(&files)?))
}

#[cfg(unix)]
pub(super) fn open_policy_model(
    definition: &crate::clipboard_policy::PolicyModel,
    path: &Path,
    cancelled: &AtomicBool,
) -> io::Result<Opened> {
    require_name(&definition.name)?;
    let (directory, mut files) = open_policy_directory(path, cancelled)?;
    let assets = files
        .iter()
        .map(|(name, file)| Ok((name.clone(), file.metadata()?.len())))
        .collect::<io::Result<_>>()?;
    let model = Model {
        id: definition.id.clone(),
        name: definition.name.clone(),
        bytes: size_of_assets(&assets)?,
        idle_minutes: DEFAULT_IDLE_MINUTES,
        assets,
    };
    let stamps = stamps(&files)?;
    validate_assets(&model, &mut files)?;
    check_cancelled(cancelled)?;
    let opened = Opened {
        model,
        path: path.to_owned(),
        directory,
        files,
        stamps,
        _lease: None,
    };
    opened.revalidate()?;
    Ok(opened)
}

fn is_asset(name: &str) -> bool {
    if !name.is_ascii()
        || name.len() > 160
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return false;
    }
    (name.starts_with("model") && name.ends_with(".safetensors"))
        || matches!(
            name,
            INDEX
                | "config.json"
                | "generation_config.json"
                | "hf_quant_config.json"
                | "tokenizer.json"
                | "tokenizer_config.json"
                | "special_tokens_map.json"
                | "added_tokens.json"
                | "chat_template.jinja"
                | "tokenizer.model"
                | "sentencepiece.bpe.model"
                | "spiece.model"
                | "tiktoken.model"
                | "vocab.json"
                | "merges.txt"
        )
}

fn validate_assets(model: &Model, files: &mut BTreeMap<String, File>) -> io::Result<()> {
    let weights: BTreeSet<_> = files
        .keys()
        .filter(|name| name.ends_with(".safetensors"))
        .cloned()
        .collect();
    if weights.is_empty()
        || !files.contains_key("config.json")
        || !(files.contains_key("tokenizer.json")
            || files.contains_key("tokenizer.model")
            || files.contains_key("sentencepiece.bpe.model")
            || files.contains_key("spiece.model")
            || files.contains_key("tiktoken.model")
            || files.contains_key("vocab.json") && files.contains_key("merges.txt"))
    {
        return Err(invalid(
            "An MLX model requires configuration, tokenizer data and safetensors weights",
        ));
    }
    if weights.len() > 1 && !files.contains_key(INDEX) {
        return Err(invalid("Split MLX weights require the safetensors index"));
    }
    for (name, file) in files {
        if weights.contains(name) {
            let mut length = [0; 8];
            file.read_exact(&mut length)?;
            let length = u64::from_le_bytes(length);
            if !(2..=MAX_METADATA).contains(&length) || length + 8 >= model.assets[name] {
                return Err(invalid("Invalid safetensors header length"));
            }
            let header = read_json(file.take(length))?;
            if !header
                .as_object()
                .is_some_and(|values| values.keys().any(|name| name != "__metadata__"))
            {
                return Err(invalid("Missing safetensors tensor metadata"));
            }
        } else if name.ends_with(".json") {
            let value = read_json(file.take(MAX_METADATA + 1))?;
            if matches!(name.as_str(), "config.json" | "tokenizer_config.json")
                && unsafe_configuration(&value)
            {
                return Err(invalid("MLX model configuration requests custom code"));
            }
            if name == "config.json"
                && !value
                    .get("model_type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        !kind.is_empty()
                            && kind.len() <= 96
                            && kind
                                .bytes()
                                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    })
            {
                return Err(invalid(
                    "The MLX model type must name a built-in architecture",
                ));
            }
            if name == INDEX {
                let mapped = value
                    .get("weight_map")
                    .and_then(Value::as_object)
                    .filter(|entries| !entries.is_empty())
                    .ok_or_else(|| invalid("Invalid safetensors weight map"))?
                    .values()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| invalid("Invalid safetensors shard name"))
                    })
                    .collect::<io::Result<BTreeSet<_>>>()?;
                if mapped != weights {
                    return Err(invalid(
                        "The safetensors index must name exactly the reviewed shards",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn unsafe_configuration(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            matches!(key.as_str(), "auto_map" | "custom_pipelines" | "model_file")
                || key == "trust_remote_code" && value != &Value::Bool(false)
                || unsafe_configuration(value)
        }),
        Value::Array(values) => values.iter().any(unsafe_configuration),
        _ => false,
    }
}

fn read_json(mut file: impl Read) -> io::Result<Value> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn size_of_assets(assets: &BTreeMap<String, u64>) -> io::Result<u64> {
    let mut total = 0_u64;
    if assets.len() > MAX_FILES {
        return Err(invalid("Too many MLX model files"));
    }
    for (name, size) in assets {
        let limit = if name.ends_with(".safetensors") {
            MAX_BYTES
        } else {
            MAX_METADATA
        };
        if !is_asset(name) || !(1..=limit).contains(size) {
            return Err(invalid(
                "An MLX model file is invalid or exceeds its size limit",
            ));
        }
        total = total
            .checked_add(*size)
            .filter(|total| *total <= MAX_BYTES)
            .ok_or_else(|| invalid("The MLX model exceeds its size limit"))?;
    }
    Ok(total)
}

fn read_model(directory: &Directory, id: &str) -> io::Result<Model> {
    let file = directory.open_private_file(MANIFEST)?;
    require_single_link(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
    decode_model(&bytes, id)
}

fn decode_model(bytes: &[u8], id: &str) -> io::Result<Model> {
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(invalid("The MLX model manifest exceeds its size limit"));
    }
    let model: Model = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    require_name(&model.name)?;
    if model.id != id || model.idle_minutes > 1_440 || model.bytes != size_of_assets(&model.assets)?
    {
        return Err(invalid("Invalid MLX model manifest"));
    }
    Ok(model)
}

pub(crate) fn validate_registration(bytes: &[u8], id: &str) -> io::Result<()> {
    decode_model(bytes, id).map(drop)
}

fn require_name(name: &str) -> io::Result<()> {
    if name.trim().is_empty() || name.chars().count() > 200 || name.chars().any(char::is_control) {
        return Err(invalid("Enter an MLX model name of up to 200 characters"));
    }
    Ok(())
}

fn stamps(files: &BTreeMap<String, File>) -> io::Result<BTreeMap<String, FileStamp>> {
    files
        .iter()
        .map(|(name, file)| Ok((name.clone(), FileStamp::read(file)?)))
        .collect()
}

fn copy(
    source: &mut File,
    output: &mut File,
    mut bytes: u64,
    buffer: &mut [u8],
    cancelled: &AtomicBool,
) -> io::Result<()> {
    while bytes > 0 {
        check_cancelled(cancelled)?;
        let count = bytes.min(buffer.len() as u64) as usize;
        source.read_exact(&mut buffer[..count])?;
        output.write_all(&buffer[..count])?;
        bytes -= count as u64;
    }
    if source.read(&mut [0])? != 0 {
        return Err(invalid("The reviewed MLX file grew during import"));
    }
    Ok(())
}

fn ids(root: &Directory) -> io::Result<Vec<String>> {
    root.names(MAX_MODELS + 2)?
        .into_iter()
        .filter(|name| name != LOCK && name != STAGING)
        .map(|name| {
            let id = name
                .into_string()
                .map_err(|_| invalid("Invalid MLX registry entry"))?;
            require_id(&id)?;
            Ok(id)
        })
        .collect()
}

fn lock(directory: &Directory, name: &str, shared: bool) -> io::Result<File> {
    let file = directory.open_lock(name)?;
    require_single_link(&file)?;
    if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    }
    .map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::new(io::ErrorKind::WouldBlock, "The MLX model is in use")
        }
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(file)
}

fn prepare_staging(root: &Directory) -> io::Result<Directory> {
    let staging = root.private_child(STAGING, true)?;
    for name in staging.names(MAX_MODELS)? {
        let id = name
            .to_str()
            .ok_or_else(|| invalid("Invalid MLX model stage"))?;
        require_id(id)?;
        discard_stage(&staging, id)?;
    }
    Ok(staging)
}

fn discard_stage(staging: &Directory, id: &str) -> io::Result<()> {
    let directory = staging.private_child(id, false)?;
    let identity = directory.identity()?;
    for name in directory.names(MAX_FILES + 3)? {
        let name = name
            .to_str()
            .ok_or_else(|| invalid("Invalid MLX model stage entry"))?;
        if !is_asset(name)
            && !matches!(name, MANIFEST | LEASE)
            && !name.strip_prefix(".stage-").is_some_and(valid_id)
        {
            return Err(invalid("Unexpected MLX model stage entry"));
        }
        let file = directory.open_private_file(name)?;
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        drop(file);
        directory.remove_unchanged(name, &stamp)?;
    }
    drop(directory);
    staging.remove_deletion_entry(OsStr::new(id), identity, true)?;
    staging.sync()
}

fn require_id(id: &str) -> io::Result<()> {
    if !valid_id(id) {
        return Err(invalid("Choose a registered MLX model"));
    }
    Ok(())
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "MLX model import cancelled",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> PathBuf {
        let source = root.join("Tiny MLX fixture");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("config.json"), br#"{"model_type":"llama"}"#).unwrap();
        std::fs::write(source.join("tokenizer.json"), b"{}").unwrap();
        let header = br#"{"x":{"dtype":"U8","shape":[1],"data_offsets":[0,1]}}"#;
        let mut weights = (header.len() as u64).to_le_bytes().to_vec();
        weights.extend(header);
        weights.push(1);
        std::fs::write(source.join("model.safetensors"), &weights).unwrap();
        std::fs::write(source.join("model.py"), b"must never be imported").unwrap();
        source
    }

    #[test]
    fn mlx_review_import_leases_only_data_and_detects_stale_source_and_installed_inventory() {
        let fixture_root = crate::snippets::tests::Profile::new();
        let root = fixture_root.0.canonicalize().unwrap();
        let source = fixture(&root);
        let cancelled = AtomicBool::new(false);
        let stale = Review::prepare(&source, &cancelled).unwrap();
        std::fs::write(source.join("tokenizer.json"), b"{\"version\":1}").unwrap();
        let registry = Registry::new(&root);
        assert!(registry.import(&stale, &cancelled).is_err());
        assert!(registry.list().unwrap().is_empty());
        let review = Review::prepare(&source, &cancelled).unwrap();
        assert_eq!(
            review.asset_names().collect::<Vec<_>>(),
            ["config.json", "model.safetensors", "tokenizer.json"]
        );
        let model = registry.import(&review, &cancelled).unwrap();
        let leased = registry.open(&model.id).unwrap();
        assert!(!leased.path.join("model.py").exists());
        assert_eq!(
            registry.remove(&model.id).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        std::fs::write(
            leased.path.join("unexpected.py"),
            b"must not reach the loader",
        )
        .unwrap();
        assert!(leased.revalidate().is_err());
        std::fs::remove_file(leased.path.join("unexpected.py")).unwrap();
        drop(leased);
        assert_eq!(
            registry
                .configure(&model.id, "Renamed fixture", 0)
                .unwrap()
                .idle_minutes,
            0
        );
        registry.remove(&model.id).unwrap();
        assert!(registry.list().unwrap().is_empty());
        assert!(source.join("model.safetensors").is_file());
    }

    #[test]
    fn mlx_review_rejects_remote_code_and_a_weight_map_that_escapes_the_model() {
        let fixture_root = crate::snippets::tests::Profile::new();
        let root = fixture_root.0.canonicalize().unwrap();
        let source = fixture(&root);
        let cancelled = AtomicBool::new(false);
        std::fs::write(
            source.join("config.json"),
            br#"{"model_type":"llama","model_file":"model.py"}"#,
        )
        .unwrap();
        assert!(
            Review::prepare(&source, &cancelled)
                .err()
                .expect("custom model code must fail review")
                .to_string()
                .contains("custom code")
        );
        std::fs::write(source.join("config.json"), br#"{"model_type":"llama"}"#).unwrap();
        std::fs::write(
            source.join(INDEX),
            br#"{"weight_map":{"x":"../other.safetensors"}}"#,
        )
        .unwrap();
        assert!(
            Review::prepare(&source, &cancelled)
                .err()
                .expect("escaping weight map must fail review")
                .to_string()
                .contains("reviewed shards")
        );
    }
}
