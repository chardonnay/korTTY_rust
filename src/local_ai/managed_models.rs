//! Private managed GGUF copies and model registrations.

use std::ffi::OsStr;
use std::fs::{File, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::runtime_index::PackageBackend;
use super::runtime_package::open_input;
use crate::private_files::{Directory, FileLock, FileStamp, nonce, require_single_link};

mod external;
mod gguf;
pub(crate) mod huggingface;
mod projector;
mod split;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use split::tests::fixture as split_test_fixture;

pub(crate) const BACKUP_DIRECTORY: &str = "llama-models";
const REGISTRY: &str = BACKUP_DIRECTORY;
const STAGING: &str = ".staging";
const REGISTRY_LOCK: &str = ".registry.lock";
const LEASE: &str = ".use.lock";
pub(crate) const BACKUP_MANIFEST: &str = "model-v1.json";
const MANIFEST: &str = BACKUP_MANIFEST;
const WEIGHTS: &str = "model.gguf";
pub(crate) const MAX_MODELS: usize = 256;
pub(crate) const MAX_MANIFEST: u64 = 64 * 1024;
pub(super) const MAX_MODEL_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 200;
const MAX_CPU_THREADS: u16 = 1024;
pub(super) const MAX_GPU_LAYERS: i16 = 10_000;
const MAX_IDLE_TIMEOUT_MINUTES: u32 = 1440;
pub(crate) const AUTO_GPU_LAYERS: i16 = -1;
pub(crate) const DEFAULT_IDLE_TIMEOUT_MINUTES: u32 = 10;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Purpose {
    #[default]
    Chat,
    Embedding,
}

impl Purpose {
    pub(crate) const ALL: [Self; 2] = [Self::Chat, Self::Embedding];

    fn is_chat(&self) -> bool {
        *self == Self::Chat
    }

    pub(super) fn require(self, expected: Option<Self>) -> io::Result<()> {
        if expected.is_some_and(|expected| expected != self) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The managed model is registered for a different purpose",
            ));
        }
        Ok(())
    }
}

impl std::fmt::Display for Purpose {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Chat => "Chat",
            Self::Embedding => "Embeddings",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelBackend {
    #[default]
    Auto,
    Cpu,
    Metal,
    Vulkan,
}

impl ModelBackend {
    pub(crate) const ALL: [Self; 4] = [Self::Auto, Self::Cpu, Self::Metal, Self::Vulkan];

    fn is_auto(&self) -> bool {
        *self == Self::Auto
    }

    pub(crate) fn required_package(self) -> Option<PackageBackend> {
        match self {
            Self::Auto | Self::Cpu => None,
            Self::Metal => Some(PackageBackend::Metal),
            Self::Vulkan => Some(PackageBackend::Vulkan),
        }
    }

    pub(crate) fn supported(self) -> bool {
        match self {
            Self::Auto | Self::Cpu => true,
            Self::Metal => cfg!(target_os = "macos"),
            Self::Vulkan => cfg!(any(target_os = "windows", target_os = "linux")),
        }
    }

    pub(crate) fn available(self, active: Option<PackageBackend>) -> bool {
        self.supported()
            && self
                .required_package()
                .is_none_or(|required| Some(required) == active)
    }
}

impl std::fmt::Display for ModelBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Auto => "Auto",
            Self::Cpu => "CPU",
            Self::Metal => "Metal",
            Self::Vulkan => "Vulkan",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Model {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) bytes: u64,
    #[serde(default, skip_serializing_if = "ModelBackend::is_auto")]
    pub(crate) backend: ModelBackend,
    pub(crate) context_tokens: u32,
    #[serde(default, skip_serializing_if = "automatic_threads")]
    pub(crate) cpu_threads: u16,
    #[serde(
        default = "auto_gpu_layers",
        skip_serializing_if = "automatic_gpu_layers"
    )]
    pub(crate) gpu_layers: i16,
    #[serde(default, skip_serializing_if = "Purpose::is_chat")]
    pub(crate) purpose: Purpose,
    #[serde(default = "default_idle_timeout_minutes")]
    pub(crate) idle_timeout_minutes: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) shard_bytes: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) external: Option<external::Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) projector_bytes: Option<u64>,
}

impl Model {
    fn new(name: String, bytes: u64, context: u32, purpose: Purpose) -> io::Result<Self> {
        let context_tokens = match purpose {
            Purpose::Chat => context,
            Purpose::Embedding => context.min(4096),
        };
        Ok(Self {
            id: nonce()?,
            name,
            bytes,
            backend: ModelBackend::Auto,
            context_tokens,
            cpu_threads: 0,
            gpu_layers: AUTO_GPU_LAYERS,
            purpose,
            idle_timeout_minutes: DEFAULT_IDLE_TIMEOUT_MINUTES,
            shard_bytes: Vec::new(),
            external: None,
            projector_bytes: None,
        })
    }

    pub(crate) fn validate_settings(&self) -> io::Result<()> {
        if let Some(bytes) = self.projector_bytes
            && (self.purpose != Purpose::Chat
                || self.external.is_some()
                || !(1..=projector::MAX_BYTES).contains(&bytes)
                || self
                    .bytes
                    .checked_add(bytes)
                    .is_none_or(|total| total > MAX_MODEL_BYTES))
        {
            return Err(invalid(
                "A vision projector requires a private chat model within the total size limit",
            ));
        }
        if self.name.trim().is_empty()
            || self.name.chars().count() > MAX_NAME_CHARS
            || self.name.chars().any(char::is_control)
        {
            return Err(invalid(
                "Model name must contain between 1 and 200 characters",
            ));
        }
        if !(gguf::MIN_CONTEXT..=gguf::MAX_CONTEXT).contains(&self.context_tokens) {
            return Err(invalid(
                "Context size must be between 512 and 2097152 tokens",
            ));
        }
        if self.cpu_threads > MAX_CPU_THREADS {
            return Err(invalid(
                "CPU threads must be Automatic (0) or between 1 and 1024",
            ));
        }
        if !(AUTO_GPU_LAYERS..=MAX_GPU_LAYERS).contains(&self.gpu_layers) {
            return Err(invalid(
                "GPU layers must be Automatic (-1) or between 0 and 10000",
            ));
        }
        if self.idle_timeout_minutes > MAX_IDLE_TIMEOUT_MINUTES {
            return Err(invalid(
                "Idle timeout must be Never (0) or between 1 and 1440 minutes",
            ));
        }
        Ok(())
    }
}

fn automatic_threads(threads: &u16) -> bool {
    *threads == 0
}

fn auto_gpu_layers() -> i16 {
    AUTO_GPU_LAYERS
}

fn automatic_gpu_layers(layers: &i16) -> bool {
    *layers == AUTO_GPU_LAYERS
}

fn default_idle_timeout_minutes() -> u32 {
    DEFAULT_IDLE_TIMEOUT_MINUTES
}

pub(crate) struct OpenedModel {
    pub(crate) model: Model,
    pub(crate) path: PathBuf,
    storage: OpenedStorage,
}

enum OpenedStorage {
    Private {
        directory: Directory,
        weights: Vec<Weight>,
        _lease: FileLock,
    },
    External {
        files: Vec<external::Lease>,
        authority: external::Authority,
        _registration: Option<(Directory, FileLock)>,
    },
    #[cfg(target_os = "linux")]
    ImmutablePolicy(super::policy_models::immutable::Protected),
}

struct Weight {
    name: String,
    file: File,
    stamp: FileStamp,
}

impl OpenedModel {
    pub(crate) fn stops_when_idle(&self) -> bool {
        #[cfg(target_os = "linux")]
        if let OpenedStorage::ImmutablePolicy(weights) = &self.storage {
            return weights.is_memory_backed();
        }
        // Darwin reopens /dev/fd/0 at the inherited file description's
        // current offset. Restart the sidecar with a fresh lease on wake.
        cfg!(target_os = "macos")
            && matches!(
                &self.storage,
                OpenedStorage::External {
                    authority: external::Authority::AdministratorCache,
                    ..
                }
            )
    }

    pub(crate) fn configure_launch(&self, command: &mut std::process::Command) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let OpenedStorage::ImmutablePolicy(weights) = &self.storage {
            use std::io::{Seek as _, SeekFrom};
            let mut file = weights.file().try_clone()?;
            file.seek(SeekFrom::Start(0))?;
            command.stdin(std::process::Stdio::from(file));
            command.arg("--model").arg("/proc/self/fd/0");
            return Ok(());
        }
        if let OpenedStorage::External {
            files, authority, ..
        } = &self.storage
        {
            self.model
                .external
                .as_ref()
                .ok_or_else(|| invalid("An external model requires a source reference"))?
                .configure_launch(files, *authority, command)
        } else {
            command.arg("--model").arg(&self.path);
            if self.model.projector_bytes.is_some() {
                command
                    .arg("--mmproj")
                    .arg(self.path.with_file_name(projector::NAME));
            }
            Ok(())
        }
    }

    /// Verify the pathname given to the sidecar still resolves to the leased weights.
    pub(crate) fn revalidate(&self) -> io::Result<()> {
        let (original, weights) = match &self.storage {
            OpenedStorage::Private {
                directory, weights, ..
            } => (directory, weights),
            OpenedStorage::External {
                files, authority, ..
            } => {
                return self
                    .model
                    .external
                    .as_ref()
                    .ok_or_else(|| invalid("An external model requires a source reference"))?
                    .revalidate(files, *authority);
            }
            #[cfg(target_os = "linux")]
            OpenedStorage::ImmutablePolicy(_) => return Ok(()),
        };
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("Managed model path has no parent"))?;
        let directory = Directory::open(parent, false)?;
        if !original.same_as(&directory)? {
            return Err(invalid("Managed model directory changed"));
        }
        for weight in weights {
            let current = directory.open_private_file(&weight.name)?;
            require_single_link(&current)?;
            weight.stamp.require_unchanged(&weight.file)?;
            weight.stamp.require_unchanged(&current)?;
        }
        Ok(())
    }

    pub(super) fn policy_stamps(&self) -> io::Result<impl Iterator<Item = &FileStamp>> {
        let OpenedStorage::External { files, .. } = &self.storage else {
            return Err(invalid("A policy model requires an external source"));
        };
        Ok(files.iter().map(external::Lease::stamp))
    }

    pub(super) fn policy_cache_file(&self) -> io::Result<&File> {
        if let OpenedStorage::External {
            files,
            authority: external::Authority::VerifiedCache | external::Authority::AdministratorCache,
            ..
        } = &self.storage
            && let [file] = files.as_slice()
        {
            return Ok(file.file());
        }
        Err(invalid(
            "A verified remote model requires one complete GGUF file",
        ))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn into_immutable_policy(
        mut self,
        weights: super::policy_models::immutable::Protected,
    ) -> Self {
        self.model.external = None;
        self.storage = OpenedStorage::ImmutablePolicy(weights);
        self
    }
}

pub(super) fn policy_source_stamps(path: &Path) -> io::Result<Vec<FileStamp>> {
    external::source_stamps(path, external::Authority::Administrator)
}

pub(super) fn open_policy_model(
    definition: &crate::clipboard_policy::PolicyModel,
    path: &Path,
    purpose: Purpose,
    cancelled: &AtomicBool,
) -> io::Result<OpenedModel> {
    open_policy_source(
        definition,
        path,
        purpose,
        cancelled,
        external::Authority::Administrator,
    )
}

pub(super) fn open_policy_cache_model(
    definition: &crate::clipboard_policy::PolicyModel,
    path: &Path,
    purpose: Purpose,
    cancelled: &AtomicBool,
) -> io::Result<OpenedModel> {
    open_policy_source(
        definition,
        path,
        purpose,
        cancelled,
        external::Authority::VerifiedCache,
    )
}

pub(super) fn open_administrator_cache_model(
    definition: &crate::clipboard_policy::PolicyModel,
    path: &Path,
    purpose: Purpose,
    cancelled: &AtomicBool,
) -> io::Result<OpenedModel> {
    if split::is_candidate(path) {
        return Err(invalid(
            "A protected remote cache requires one complete GGUF file",
        ));
    }
    open_policy_source(
        definition,
        path,
        purpose,
        cancelled,
        external::Authority::AdministratorCache,
    )
}

pub(super) fn validate_policy_cache_file(
    file: &mut File,
    bytes: u64,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    gguf::context(file, bytes, cancelled).map(|_| ())
}

#[cfg(test)]
pub(super) fn policy_cache_fixture(context: u32) -> Vec<u8> {
    tests::fixture(context, 0)
}

fn open_policy_source(
    definition: &crate::clipboard_policy::PolicyModel,
    path: &Path,
    purpose: Purpose,
    cancelled: &AtomicBool,
    authority: external::Authority,
) -> io::Result<OpenedModel> {
    let (mut model, files) =
        external::Reference::prepare_model(path, purpose, cancelled, authority)?;
    model.id = definition.id.clone();
    model.name = definition.name.clone();
    model.validate_settings()?;
    let path = model
        .external
        .as_ref()
        .ok_or_else(|| invalid("A policy model requires an external source"))?
        .path()
        .to_owned();
    let opened = OpenedModel {
        model,
        path,
        storage: OpenedStorage::External {
            files,
            authority,
            _registration: None,
        },
    };
    opened.revalidate()?;
    Ok(opened)
}

#[cfg(unix)]
pub(super) use external::require_local as require_local_source;

#[derive(Clone)]
pub(crate) struct Registry {
    profile: PathBuf,
}

impl Registry {
    pub(crate) fn new(profile: PathBuf) -> Self {
        Self { profile }
    }

    pub(crate) fn model_name(&self, id: &str) -> io::Result<String> {
        check_id(id)?;
        let root = self.root(false)?;
        let _lock = lock(&root, REGISTRY_LOCK, true)?;
        read_model(&root.private_child(id, false)?, id).map(|model| model.name)
    }

    pub(crate) fn list(&self) -> io::Result<Vec<Model>> {
        let root = match self.root(false) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let _lock = lock(&root, REGISTRY_LOCK, true)?;
        let mut models = Vec::new();
        for id in model_ids(&root)? {
            let directory = root.private_child(&id, false)?;
            models.push(read_model(&directory, &id)?);
        }
        models.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Ok(models)
    }

    pub(crate) fn import(
        &self,
        path: &Path,
        purpose: Purpose,
        cancelled: &AtomicBool,
    ) -> io::Result<Model> {
        self.import_components(path, purpose, None, cancelled)
    }

    pub(crate) fn import_projector_pair(
        &self,
        path: &Path,
        projector_path: &Path,
        cancelled: &AtomicBool,
    ) -> io::Result<Model> {
        check_cancelled(cancelled)?;
        let projector = projector::Projector::open(projector_path, cancelled)?;
        self.import_components(path, Purpose::Chat, Some(projector), cancelled)
    }

    fn import_components(
        &self,
        path: &Path,
        purpose: Purpose,
        projector: Option<projector::Projector>,
        cancelled: &AtomicBool,
    ) -> io::Result<Model> {
        check_cancelled(cancelled)?;
        if split::is_candidate(path) {
            let mut group = split::Group::open(path, cancelled)?;
            let mut model = Model::new(group.name.clone(), group.bytes, group.context, purpose)?;
            model.shard_bytes = group.sizes();
            return self.publish_import(model, projector, cancelled, |stage| {
                group.copy_into(stage, cancelled)
            });
        }
        let name = import_name(path)?;
        let mut source = open_input(path)?;
        let stamp = FileStamp::read(&source)?;
        let bytes = source.metadata()?.len();
        if !(1..=MAX_MODEL_BYTES).contains(&bytes) {
            return Err(invalid("GGUF size exceeds the model import limit"));
        }
        let context = gguf::context(&mut source, bytes, cancelled)?;
        source.rewind()?;
        let model = Model::new(name, bytes, context, purpose)?;
        self.publish_import(model, projector, cancelled, |stage| {
            stage.publish_with(WEIGHTS, |output| {
                copy_model(&mut source, output, bytes, cancelled)?;
                stamp.require_unchanged(&source)
            })
        })
    }

    fn publish_import(
        &self,
        mut model: Model,
        mut projector: Option<projector::Projector>,
        cancelled: &AtomicBool,
        copy: impl FnOnce(&Directory) -> io::Result<()>,
    ) -> io::Result<Model> {
        model.projector_bytes = projector.as_ref().map(|projector| projector.bytes);
        model.validate_settings()?;
        let root = self.root(true)?;
        let _lock = lock(&root, REGISTRY_LOCK, false)?;
        let staging = prepare_staging(&root)?;
        if model_ids(&root)?.len() >= MAX_MODELS {
            return Err(invalid("Managed model registry is full"));
        }
        check_cancelled(cancelled)?;
        let stage = staging.create_private_subdirectory(OsStr::new(&model.id))?;
        let prepared = (|| {
            copy(&stage)?;
            if let Some(projector) = &mut projector {
                projector.copy_into(&stage, cancelled)?;
            }
            stage.publish_copy(
                MANIFEST,
                &serde_json::to_vec(&model).map_err(io::Error::other)?,
            )?;
            stage.sync()?;
            check_cancelled(cancelled)
        })();
        drop(stage);
        let published = prepared.and_then(|()| staging.move_subdirectory(&model.id, &root));
        if let Err(error) = published {
            let _ = discard_stage(&staging, &model.id);
            return Err(error);
        }
        root.sync()?;
        staging.sync()?;
        Ok(model)
    }

    pub(crate) fn reference(
        &self,
        path: &Path,
        purpose: Purpose,
        cancelled: &AtomicBool,
    ) -> io::Result<Model> {
        let (model, leases) = external::Reference::register_model(path, purpose, cancelled)?;
        let reference = model.external.clone().expect("registered external model");
        self.publish_import(model, None, cancelled, |_| {
            reference.revalidate(&leases, external::Authority::User)
        })
    }

    pub(crate) fn open(&self, id: &str) -> io::Result<OpenedModel> {
        check_id(id)?;
        let root = self.root(false)?;
        let _lock = lock(&root, REGISTRY_LOCK, true)?;
        let directory = root.private_child(id, false)?;
        let lease = lock(&directory, LEASE, true)?;
        let model = read_model(&directory, id)?;
        if let Some(reference) = &model.external {
            let external = reference.open()?;
            let path = reference.path().to_owned();
            return Ok(OpenedModel {
                model,
                path,
                storage: OpenedStorage::External {
                    files: external,
                    authority: external::Authority::User,
                    _registration: Some((directory, lease)),
                },
            });
        }
        let mut components = if model.shard_bytes.is_empty() {
            vec![(WEIGHTS.to_owned(), model.bytes)]
        } else {
            let count = model.shard_bytes.len();
            model
                .shard_bytes
                .iter()
                .enumerate()
                .map(|(index, &size)| (split::private_name(index, count), size))
                .collect()
        };
        if let Some(bytes) = model.projector_bytes {
            components.push((projector::NAME.to_owned(), bytes));
        }
        let mut weights = Vec::with_capacity(components.len());
        for (name, size) in components {
            let file = directory.open_private_file(&name).map_err(|error| {
                if name == projector::NAME {
                    io::Error::new(
                        error.kind(),
                        "The registered vision projector is unavailable",
                    )
                } else {
                    error
                }
            })?;
            require_single_link(&file)?;
            let stamp = FileStamp::read(&file)?;
            if file.metadata()?.len() != size {
                return Err(invalid("Managed model size changed"));
            }
            weights.push(Weight { name, file, stamp });
        }
        let path = self.profile.join(REGISTRY).join(id).join(&weights[0].name);
        Ok(OpenedModel {
            model,
            path,
            storage: OpenedStorage::Private {
                directory,
                weights,
                _lease: lease,
            },
        })
    }

    pub(crate) fn configure(&self, mut model: Model) -> io::Result<Model> {
        check_id(&model.id)?;
        model.name = model.name.trim().to_owned();
        model.validate_settings()?;
        let root = self.root(false)?;
        let _lock = lock(&root, REGISTRY_LOCK, false)?;
        let directory = root.private_child(&model.id, false)?;
        let _lease = lock(&directory, LEASE, false)?;
        let stored = read_model(&directory, &model.id)?;
        model.bytes = stored.bytes;
        model.shard_bytes = stored.shard_bytes;
        model.external = stored.external;
        model.projector_bytes = stored.projector_bytes;
        model.validate_settings()?;
        let bytes = serde_json::to_vec(&model).map_err(io::Error::other)?;
        directory.replace_file(MANIFEST, &bytes)?;
        directory.sync()?;
        Ok(model)
    }

    pub(crate) fn remove(&self, id: &str) -> io::Result<()> {
        check_id(id)?;
        let root = self.root(false)?;
        let _lock = lock(&root, REGISTRY_LOCK, false)?;
        let directory = root.private_child(id, false)?;
        let lease = lock(&directory, LEASE, false)?;
        let staging = prepare_staging(&root)?;
        // The registry lock prevents another opener while the model is being unpublished.
        drop(lease);
        drop(directory);
        root.move_subdirectory(id, &staging)?;
        root.sync()?;
        staging.sync()?;
        discard_stage(&staging, id)
    }

    fn root(&self, create: bool) -> io::Result<Directory> {
        Directory::open(&self.profile, create)?.private_child(REGISTRY, create)
    }
}

fn read_model(directory: &Directory, id: &str) -> io::Result<Model> {
    let file = directory.open_private_file(MANIFEST)?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file).take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    decode_model(&bytes, id)
}

fn decode_model(bytes: &[u8], id: &str) -> io::Result<Model> {
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(invalid("Managed model registration exceeds its size limit"));
    }
    let model: Model = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    if model.id != id
        || !(1..=MAX_MODEL_BYTES).contains(&model.bytes)
        || (!model.shard_bytes.is_empty() && !split::valid_sizes(&model.shard_bytes, model.bytes))
    {
        return Err(invalid("Invalid managed model registration"));
    }
    model.validate_settings()?;
    if let Some(reference) = &model.external {
        reference.validate(model.bytes, &model.shard_bytes)?;
    }
    Ok(model)
}

pub(crate) fn portable_registration(bytes: &[u8], id: &str) -> io::Result<Vec<u8>> {
    let mut model = decode_model(bytes, id)?;
    // The receiving profile must review an external model before it can use it.
    model.external = None;
    serde_json::to_vec(&model).map_err(io::Error::other)
}

fn import_name(path: &Path) -> io::Result<String> {
    if !path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
    {
        return Err(invalid("Choose a .gguf model file"));
    }
    let name = path
        .file_stem()
        .and_then(OsStr::to_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
        .ok_or_else(|| invalid("GGUF model has an invalid file name"))?;
    Ok(name.chars().take(MAX_NAME_CHARS).collect())
}

fn copy_model(
    source: &mut File,
    output: &mut File,
    mut bytes: u64,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut buffer = vec![0; 128 * 1024];
    while bytes > 0 {
        check_cancelled(cancelled)?;
        let size = bytes.min(buffer.len() as u64) as usize;
        source.read_exact(&mut buffer[..size])?;
        output.write_all(&buffer[..size])?;
        bytes -= size as u64;
    }
    if source.read(&mut [0])? != 0 {
        return Err(invalid("GGUF model grew during import"));
    }
    Ok(())
}

fn lock(directory: &Directory, name: &str, shared: bool) -> io::Result<FileLock> {
    let file = directory.open_lock(name)?;
    require_single_link(&file)?;
    let result = if shared {
        file.try_lock_shared()
    } else {
        file.try_lock()
    };
    result.map_err(|error| match error {
        TryLockError::WouldBlock => {
            let message = if name == LEASE {
                "Stop the managed model before changing or removing its registration"
            } else {
                "Another managed model operation is running"
            };
            io::Error::new(io::ErrorKind::WouldBlock, message)
        }
        TryLockError::Error(error) => error,
    })?;
    Ok(FileLock::new(file))
}

fn model_ids(root: &Directory) -> io::Result<Vec<String>> {
    root.names(MAX_MODELS + 2)?
        .into_iter()
        .filter(|name| name != REGISTRY_LOCK && name != STAGING)
        .map(|name| {
            let id = name
                .into_string()
                .map_err(|_| invalid("Invalid managed model ID"))?;
            check_id(&id)?;
            Ok(id)
        })
        .collect()
}

fn prepare_staging(root: &Directory) -> io::Result<Directory> {
    let staging = root.private_child(STAGING, true)?;
    // Only the process holding the exclusive registry lock can have a live stage.
    for name in staging.names(MAX_MODELS)? {
        let id = name
            .to_str()
            .ok_or_else(|| invalid("Invalid model stage name"))?;
        check_id(id)?;
        discard_stage(&staging, id)?;
    }
    Ok(staging)
}

fn discard_stage(staging: &Directory, id: &str) -> io::Result<()> {
    let directory = staging.private_child(id, false)?;
    let identity = directory.identity()?;
    for name in directory.names(split::MAX_SHARDS + 5)? {
        let name = name
            .to_str()
            .ok_or_else(|| invalid("Invalid model stage entry"))?;
        if !matches!(name, MANIFEST | WEIGHTS | LEASE | projector::NAME)
            && !split::is_private_name(name)
            && !name.strip_prefix(".stage-").is_some_and(valid_id)
        {
            return Err(invalid("Unexpected model stage entry"));
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

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn check_id(id: &str) -> io::Result<()> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(invalid("Invalid managed model ID"))
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Import cancelled",
        ))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
