use std::fs;

use super::*;

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kortty-managed-models-{}", nonce().unwrap()));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        let path = path.canonicalize().unwrap();
        Self(path)
    }

    fn registry(&self) -> Registry {
        Registry::new(self.0.clone())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn string(file: &mut Vec<u8>, value: &str) {
    file.extend_from_slice(&(value.len() as u64).to_le_bytes());
    file.extend_from_slice(value.as_bytes());
}

// A minimal GGUF fixture with a metadata array and one F32 tensor.
pub(super) fn fixture(context: u32, split: u16) -> Vec<u8> {
    let mut file = b"GGUF".to_vec();
    file.extend_from_slice(&3_u32.to_le_bytes());
    file.extend_from_slice(&1_u64.to_le_bytes());
    file.extend_from_slice(&3_u64.to_le_bytes());
    string(&mut file, "tokenizer.ggml.tokens");
    file.extend_from_slice(&9_u32.to_le_bytes());
    file.extend_from_slice(&8_u32.to_le_bytes());
    file.extend_from_slice(&1_u64.to_le_bytes());
    string(&mut file, "test");
    string(&mut file, "llama.context_length");
    file.extend_from_slice(&4_u32.to_le_bytes());
    file.extend_from_slice(&context.to_le_bytes());
    string(&mut file, "split.count");
    file.extend_from_slice(&2_u32.to_le_bytes());
    file.extend_from_slice(&split.to_le_bytes());
    string(&mut file, "test.weight");
    file.extend_from_slice(&1_u32.to_le_bytes());
    file.extend_from_slice(&1_u64.to_le_bytes());
    file.extend_from_slice(&0_u32.to_le_bytes());
    file.extend_from_slice(&0_u64.to_le_bytes());
    file.resize(file.len().next_multiple_of(32), 0);
    file.extend_from_slice(&1_f32.to_le_bytes());
    file
}

#[test]
fn managed_import_is_durable_and_removal_requires_idle_weights() {
    let profile = Profile::new();
    let path = profile.0.join("Größe.GGUF");
    let bytes = fixture(2048, 1);
    fs::write(&path, &bytes).unwrap();

    let registry = profile.registry();
    assert!(registry.list().unwrap().is_empty());
    let imported = registry
        .import(&path, Purpose::Chat, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(imported.name, "Größe");
    assert_eq!(imported.context_tokens, 2048);
    assert_eq!(imported.idle_timeout_minutes, DEFAULT_IDLE_TIMEOUT_MINUTES);
    assert_eq!(imported.bytes, bytes.len() as u64);

    let reopened = profile.registry();
    assert_eq!(
        reopened.list().unwrap().as_slice(),
        std::slice::from_ref(&imported)
    );
    let leased = reopened.open(&imported.id).unwrap();
    let other_lease = registry.open(&imported.id).unwrap();
    assert_eq!(leased.model, imported);
    assert_eq!(fs::read(&leased.path).unwrap(), bytes);
    leased.revalidate().unwrap();
    drop(leased);
    assert_eq!(
        registry.remove(&imported.id).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(other_lease);

    registry.remove(&imported.id).unwrap();
    assert!(reopened.list().unwrap().is_empty());
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!profile.0.join(REGISTRY).join(&imported.id).exists());
}

#[test]
fn managed_configuration_defaults_legacy_fields_and_preserves_leased_weights() {
    let profile = Profile::new();
    let registry = profile.registry();
    let source = profile.0.join("configure.gguf");
    let bytes = fixture(2048, 1);
    fs::write(&source, &bytes).unwrap();
    let imported = registry
        .import(&source, Purpose::Chat, &AtomicBool::new(false))
        .unwrap();
    let root = registry.root(false).unwrap();
    let directory = root.private_child(&imported.id, false).unwrap();
    let mut legacy = serde_json::to_value(&imported).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("idle_timeout_minutes");
    assert!(legacy.get("cpu_threads").is_none());
    assert!(legacy.get("gpu_layers").is_none());
    assert!(legacy.get("backend").is_none());
    directory
        .replace_file(MANIFEST, &serde_json::to_vec(&legacy).unwrap())
        .unwrap();
    assert_eq!(registry.list().unwrap()[0], imported);
    assert_eq!(imported.cpu_threads, 0);
    assert_eq!(imported.gpu_layers, AUTO_GPU_LAYERS);
    assert_eq!(imported.backend, ModelBackend::Auto);
    for (field, value, notice) in [
        ("cpu_threads", 1025, "CPU threads"),
        ("gpu_layers", 10_001, "GPU layers"),
    ] {
        let mut invalid = legacy.clone();
        invalid[field] = serde_json::json!(value);
        directory
            .replace_file(MANIFEST, &serde_json::to_vec(&invalid).unwrap())
            .unwrap();
        assert!(registry.list().unwrap_err().to_string().contains(notice));
    }
    directory
        .replace_file(MANIFEST, &serde_json::to_vec(&legacy).unwrap())
        .unwrap();

    let opened = registry.open(&imported.id).unwrap();
    assert_eq!(
        registry
            .configure(Model {
                name: "New name".into(),
                cpu_threads: 6,
                gpu_layers: 32,
                ..imported.clone()
            })
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let weights = opened.path.clone();
    drop(opened);

    for invalid in [
        Model {
            name: String::new(),
            ..imported.clone()
        },
        Model {
            context_tokens: 511,
            ..imported.clone()
        },
        Model {
            idle_timeout_minutes: 1441,
            ..imported.clone()
        },
        Model {
            cpu_threads: 1025,
            ..imported.clone()
        },
        Model {
            gpu_layers: -2,
            ..imported.clone()
        },
        Model {
            gpu_layers: 10_001,
            ..imported.clone()
        },
    ] {
        assert!(registry.configure(invalid).is_err());
    }
    assert_eq!(registry.list().unwrap()[0], imported);

    let updated = registry
        .configure(Model {
            name: "  Größe  ".into(),
            bytes: 0,
            backend: ModelBackend::Cpu,
            context_tokens: 4096,
            cpu_threads: 1024,
            gpu_layers: 10_000,
            idle_timeout_minutes: 0,
            purpose: Purpose::Embedding,
            ..imported.clone()
        })
        .unwrap();
    assert_eq!(updated.id, imported.id);
    assert_eq!(updated.bytes, imported.bytes);
    assert_eq!(updated.name, "Größe");
    assert_eq!(updated.context_tokens, 4096);
    assert_eq!(updated.cpu_threads, 1024);
    assert_eq!(updated.gpu_layers, 10_000);
    assert_eq!(updated.backend, ModelBackend::Cpu);
    assert_eq!(updated.idle_timeout_minutes, 0);
    assert_eq!(updated.purpose, Purpose::Embedding);
    assert_eq!(profile.registry().open(&updated.id).unwrap().model, updated);
    let manifest = profile.0.join(REGISTRY).join(&updated.id).join(MANIFEST);
    let zero = registry
        .configure(Model {
            gpu_layers: 0,
            ..updated
        })
        .unwrap();
    assert_eq!(registry.list().unwrap()[0], zero);
    let explicit: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(explicit["gpu_layers"], 0);
    assert_eq!(explicit["backend"], "cpu");
    let automatic = registry
        .configure(Model {
            cpu_threads: 0,
            gpu_layers: AUTO_GPU_LAYERS,
            backend: ModelBackend::Auto,
            ..zero
        })
        .unwrap();
    assert_eq!(registry.list().unwrap()[0], automatic);
    let reset: serde_json::Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert!(reset.get("cpu_threads").is_none());
    assert!(reset.get("gpu_layers").is_none());
    assert!(reset.get("backend").is_none());
    assert_eq!(fs::read(weights).unwrap(), bytes);
    assert_eq!(fs::read(source).unwrap(), bytes);
}

#[test]
fn model_backend_choices_require_a_matching_signed_package_and_supported_platform() {
    for active in [None, Some(PackageBackend::Cpu)] {
        assert!(ModelBackend::Auto.available(active));
        assert!(ModelBackend::Cpu.available(active));
        assert!(!ModelBackend::Metal.available(active));
        assert!(!ModelBackend::Vulkan.available(active));
    }
    assert_eq!(
        ModelBackend::Metal.available(Some(PackageBackend::Metal)),
        cfg!(target_os = "macos")
    );
    assert_eq!(
        ModelBackend::Vulkan.available(Some(PackageBackend::Vulkan)),
        cfg!(any(target_os = "windows", target_os = "linux"))
    );
    assert!(!ModelBackend::Vulkan.available(Some(PackageBackend::Metal)));
    assert!(!ModelBackend::Metal.available(Some(PackageBackend::Vulkan)));
}

#[test]
fn malformed_split_and_cancelled_imports_do_not_register_weights() {
    let profile = Profile::new();
    let registry = profile.registry();
    let path = profile.0.join("invalid.gguf");
    let cancelled = AtomicBool::new(false);

    fs::write(&path, b"GGUF\x03\0\0\0").unwrap();
    assert!(registry.import(&path, Purpose::Chat, &cancelled).is_err());
    let mut no_tensor_data = fixture(2048, 1);
    no_tensor_data.truncate(no_tensor_data.len() - std::mem::size_of::<f32>());
    fs::write(&path, no_tensor_data).unwrap();
    assert!(registry.import(&path, Purpose::Chat, &cancelled).is_err());
    fs::write(&path, fixture(32_768, 2)).unwrap();
    assert!(registry.import(&path, Purpose::Chat, &cancelled).is_err());
    fs::write(&path, fixture(65_536, 1)).unwrap();
    cancelled.store(true, Ordering::Release);
    assert_eq!(
        registry
            .import(&path, Purpose::Chat, &cancelled)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(registry.list().unwrap().is_empty());
    assert!(!profile.0.join(REGISTRY).exists());
}

#[test]
fn managed_lease_detects_changed_weights_before_launch() {
    let profile = Profile::new();
    let registry = profile.registry();
    let source = profile.0.join("launch.gguf");
    let bytes = fixture(65_536, 1);
    fs::write(&source, &bytes).unwrap();
    let model = registry
        .import(&source, Purpose::Chat, &AtomicBool::new(false))
        .unwrap();
    let opened = registry.open(&model.id).unwrap();
    assert_eq!(opened.model.context_tokens, 32_768);

    fs::OpenOptions::new()
        .append(true)
        .open(&opened.path)
        .unwrap()
        .write_all(b"unexpected")
        .unwrap();
    assert!(opened.revalidate().is_err());
    drop(opened);
    assert!(registry.open(&model.id).is_err());
    registry.remove(&model.id).unwrap();
    assert_eq!(fs::read(source).unwrap(), bytes);
}
