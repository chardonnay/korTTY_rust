//! Read-only embedded model sources selected by the installation policy.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pgp::crypto::hash::HashAlgorithm;

use crate::clipboard_policy::{ClipboardPolicy, PolicyModel, PolicyModelRuntime};
#[cfg(unix)]
use crate::private_files::EntryIdentity;
use crate::private_files::FileStamp;

use super::{managed_models, mlx_models};

mod http;
#[cfg(target_os = "linux")]
#[path = "policy_models/immutable/linux.rs"]
pub(super) mod immutable;
mod protected_cache;
mod remote;
mod unc;

const PREPARATION_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelIdentity([u8; 32]);

impl ModelIdentity {
    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn fixture(tag: u8) -> Self {
        Self([tag; 32])
    }
}

pub(super) fn model_identity(root: &Path, definition: &PolicyModel) -> io::Result<ModelIdentity> {
    let path = match source(definition)? {
        Source::Local(path) => path,
        Source::ProtectedCache(_, digest) => return Ok(ModelIdentity(digest)),
        Source::Remote(_) => {
            require_direct_remote()?;
            return remote::ready_identity(root, definition);
        }
    };
    match definition.runtime {
        PolicyModelRuntime::Gguf => {
            let stamps = managed_models::policy_source_stamps(path)?;
            gguf_identity(&stamps)
        }
        #[cfg(unix)]
        PolicyModelRuntime::Mlx => {
            let (directory, files) = mlx_models::policy_source_identity(path)?;
            mlx_identity(directory, &files)
        }
        #[cfg(windows)]
        PolicyModelRuntime::Mlx => Err(mlx_unsupported()),
    }
}

pub(crate) fn open_gguf(
    root: &Path,
    definition: &PolicyModel,
    purpose: managed_models::Purpose,
    expected: &ModelIdentity,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> io::Result<managed_models::OpenedModel> {
    require_runtime(definition, PolicyModelRuntime::Gguf)?;
    let authorize = || require_current(definition);
    let access = remote::Access {
        cancelled,
        deadline,
        require_current: &authorize,
    };
    let path = match source(definition)? {
        Source::Local(path) => path,
        Source::ProtectedCache(path, digest) => {
            require_identity(expected, ModelIdentity(digest))?;
            return open_protected_cache(definition, path, purpose, digest, &access);
        }
        Source::Remote(source) => {
            require_direct_remote()?;
            let opened = remote::open(root, definition, source, purpose, Some(expected), &access)?;
            #[cfg(target_os = "linux")]
            let opened = remote::protect_for_launch(opened, expected, &access)?;
            return Ok(opened);
        }
    };
    let opened = managed_models::open_policy_model(definition, path, purpose, cancelled)?;
    require_identity(expected, gguf_identity(opened.policy_stamps()?)?)?;
    Ok(opened)
}

pub(crate) fn open_mlx(
    definition: &PolicyModel,
    expected: &ModelIdentity,
    cancelled: &AtomicBool,
) -> io::Result<mlx_models::Opened> {
    require_runtime(definition, PolicyModelRuntime::Mlx)?;
    #[cfg(unix)]
    {
        let Source::Local(path) = source(definition)? else {
            return Err(remote_gguf_only());
        };
        let opened = mlx_models::open_policy_model(definition, path, cancelled)?;
        let (directory, files) = opened.policy_identity()?;
        require_identity(expected, mlx_identity(directory, files)?)?;
        Ok(opened)
    }
    #[cfg(windows)]
    {
        let _ = (expected, cancelled);
        Err(mlx_unsupported())
    }
}

/// Resolves installed-policy model sources on a worker before listing a choice.
pub(crate) fn available(definition: &PolicyModel, root: &Path, cancelled: &AtomicBool) -> bool {
    let Ok(source) = source(definition) else {
        return false;
    };
    let path = match source {
        Source::Local(path) => path,
        Source::ProtectedCache(path, digest) => {
            return open_protected_cache(
                definition,
                path,
                managed_models::Purpose::Chat,
                digest,
                &remote::Access {
                    cancelled,
                    deadline: Instant::now() + PREPARATION_TIMEOUT,
                    require_current: &|| require_current(definition),
                },
            )
            .is_ok();
        }
        Source::Remote(source) => {
            return prepare_remote(root, definition, source, cancelled).is_ok();
        }
    };
    match definition.runtime {
        PolicyModelRuntime::Gguf => managed_models::open_policy_model(
            definition,
            path,
            managed_models::Purpose::Chat,
            cancelled,
        )
        .is_ok(),
        #[cfg(unix)]
        PolicyModelRuntime::Mlx => {
            super::mlx_runtime::supported()
                && mlx_models::open_policy_model(definition, path, cancelled).is_ok()
        }
        #[cfg(windows)]
        PolicyModelRuntime::Mlx => false,
    }
}

fn open_protected_cache(
    definition: &PolicyModel,
    path: &Path,
    purpose: managed_models::Purpose,
    digest: [u8; 32],
    access: &remote::Access<'_>,
) -> io::Result<managed_models::OpenedModel> {
    access.authorize()?;
    let opened = managed_models::open_administrator_cache_model(
        definition,
        path,
        purpose,
        access.cancelled,
    )?;
    remote::verify_opened(&opened, digest, access)?;
    Ok(opened)
}

fn prepare_remote(
    root: &Path,
    definition: &PolicyModel,
    source: remote::Source<'_>,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    require_direct_remote()?;
    remote::open(
        root,
        definition,
        source,
        managed_models::Purpose::Chat,
        None,
        &remote::Access {
            cancelled,
            deadline: Instant::now() + PREPARATION_TIMEOUT,
            require_current: &|| require_current(definition),
        },
    )
    .map(drop)
}

fn require_current(definition: &PolicyModel) -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_none()
        && policy.ai_allowed
        && policy.ai_runtime_models().contains(definition)
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The policy model declaration changed or was disabled",
        ))
    }
}

enum Source<'a> {
    Local(&'a Path),
    ProtectedCache(&'a Path, [u8; 32]),
    Remote(remote::Source<'a>),
}

fn source(definition: &PolicyModel) -> io::Result<Source<'_>> {
    let source = definition.source.as_str();
    if source.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    }) {
        require_remote_gguf(definition)?;
        http::validate_url(source, definition.digest.is_some())?;
        return remote_source(definition, remote::Source::Http(source));
    }
    let path = Path::new(source);
    #[cfg(windows)]
    let local_volume = matches!(path.components().next(), Some(Component::Prefix(prefix)) if matches!(
        prefix.kind(),
        std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
    ));
    #[cfg(not(windows))]
    let local_volume = false;
    if source.starts_with(r"\\") && !local_volume {
        require_remote_gguf(definition)?;
        unc::validate(source)?;
        let source = remote_source(definition, remote::Source::Unc(source))?;
        if !cfg!(windows) && matches!(source, Source::Remote(_)) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "UNC model sources require the Windows SMB redirector",
            ));
        }
        if definition.digest.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "UNC policy model sources require a configured SHA-256 digest",
            ));
        }
        return Ok(source);
    }
    #[cfg(windows)]
    if !local_volume {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Policy model sources require a supported local volume",
        ));
    }
    if definition.digest.is_some() || definition.protected_cache.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Configured SHA-256 and protected-cache apply to remote policy model sources",
        ));
    }
    if !path.is_absolute()
        || source.len() > 4096
        || source.chars().any(char::is_control)
        || path.components().any(|part| part == Component::ParentDir)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A policy model requires an ordinary absolute source path",
        ));
    }
    Ok(Source::Local(path))
}

fn remote_source<'a>(
    definition: &'a PolicyModel,
    source: remote::Source<'a>,
) -> io::Result<Source<'a>> {
    if let Some(path) = protected_cache::path(definition)? {
        let digest = definition.digest.expect("validated protected cache digest");
        Ok(Source::ProtectedCache(path, digest))
    } else {
        Ok(Source::Remote(source))
    }
}

fn require_direct_remote() -> io::Result<()> {
    if cfg!(target_os = "macos") {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Remote policy models on macOS require an administrator-controlled protected-cache and SHA-256",
        ))
    } else {
        Ok(())
    }
}

fn require_remote_gguf(definition: &PolicyModel) -> io::Result<()> {
    if definition.runtime == PolicyModelRuntime::Gguf {
        Ok(())
    } else {
        Err(remote_gguf_only())
    }
}

fn remote_gguf_only() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Remote policy model sources require a complete GGUF file",
    )
}

pub(crate) struct StartupPreparation {
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for StartupPreparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn prepare_on_startup(
    root: PathBuf,
    policy: &ClipboardPolicy,
) -> Option<StartupPreparation> {
    require_direct_remote().ok()?;
    if !policy.ai_allowed {
        return None;
    }
    let models: Vec<_> = policy
        .ai_runtime_models()
        .iter()
        .filter(|model| matches!(source(model), Ok(Source::Remote(remote::Source::Http(_)))))
        .cloned()
        .collect();
    if models.is_empty() {
        return None;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let worker = std::thread::Builder::new()
        .name("kortty-policy-models".into())
        .spawn(move || {
            for model in models {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let _ = available(&model, &root, &stop);
            }
        })
        .ok()?;
    Some(StartupPreparation {
        cancelled,
        worker: Some(worker),
    })
}

fn require_runtime(definition: &PolicyModel, runtime: PolicyModelRuntime) -> io::Result<()> {
    if definition.runtime != runtime {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The policy model belongs to a different runtime",
        ));
    }
    Ok(())
}

fn require_identity(expected: &ModelIdentity, opened: ModelIdentity) -> io::Result<()> {
    if *expected != opened {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The policy model source changed. Review the model before starting it.",
        ));
    }
    Ok(())
}

fn gguf_identity<'a>(stamps: impl IntoIterator<Item = &'a FileStamp>) -> io::Result<ModelIdentity> {
    let mut bytes = b"kortty-policy-model-source-v1\0gguf\0".to_vec();
    for stamp in stamps {
        stamp.append_identity(&mut bytes);
    }
    digest_identity(&bytes)
}

#[cfg(unix)]
fn mlx_identity(
    directory: EntryIdentity,
    files: &BTreeMap<String, FileStamp>,
) -> io::Result<ModelIdentity> {
    let mut bytes = b"kortty-policy-model-source-v1\0mlx\0".to_vec();
    bytes.extend(directory.0.to_le_bytes());
    bytes.extend(directory.1.to_le_bytes());
    for (name, stamp) in files {
        bytes.extend((name.len() as u32).to_le_bytes());
        bytes.extend(name.as_bytes());
        stamp.append_identity(&mut bytes);
    }
    digest_identity(&bytes)
}

fn digest_identity(bytes: &[u8]) -> io::Result<ModelIdentity> {
    HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(io::Error::other)?
        .try_into()
        .map(ModelIdentity)
        .map_err(|_| io::Error::other("Unexpected policy model identity digest size"))
}

#[cfg(windows)]
fn mlx_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Managed MLX requires Apple Silicon and macOS 14 or later",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_model_protected_cache_selects_the_pinned_local_copy() {
        #[cfg(windows)]
        let cache = r"C:\Program Files\kortty\probe.gguf";
        #[cfg(unix)]
        let cache = "/opt/kortty-models/probe.gguf";
        let digest = [7; 32];
        let mut model = PolicyModel {
            id: "policy-probe".into(),
            name: "Probe".into(),
            runtime: PolicyModelRuntime::Gguf,
            source: "https://models.example.invalid/probe.gguf".into(),
            digest: Some(digest),
            protected_cache: Some(cache.into()),
        };
        assert!(matches!(
            source(&model),
            Ok(Source::ProtectedCache(path, pin)) if path == Path::new(cache) && pin == digest
        ));
        assert_eq!(
            model_identity(Path::new("unused-profile"), &model).unwrap(),
            ModelIdentity(digest)
        );
        model.source = cache.into();
        assert!(source(&model).is_err());
    }

    #[test]
    fn policy_model_sources_require_trusted_remote_transport_and_ordinary_local_paths() {
        let mut model = PolicyModel {
            id: "policy-probe".into(),
            name: "Probe".into(),
            runtime: PolicyModelRuntime::Gguf,
            source: "HTTP://models.example.invalid/probe.gguf".into(),
            digest: None,
            protected_cache: None,
        };
        assert_eq!(
            source(&model).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        model.digest = Some([1; 32]);
        assert!(matches!(
            source(&model),
            Ok(Source::Remote(remote::Source::Http(_)))
        ));
        model.digest = None;
        model.source = "https://models.example.invalid/probe.gguf".into();
        assert!(matches!(
            source(&model),
            Ok(Source::Remote(remote::Source::Http(_)))
        ));
        model.runtime = PolicyModelRuntime::Mlx;
        assert_eq!(
            source(&model).err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
        model.source = "../probe.gguf".into();
        assert_eq!(
            source(&model).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
