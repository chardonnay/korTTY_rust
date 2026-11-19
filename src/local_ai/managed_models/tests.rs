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
pub(crate) fn fixture(context: u32, split: u16) -> Vec<u8> {
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
fn restored_gguf_metadata_keeps_its_id_and_settings_on_reattachment() {
    let original = Profile::new();
    let restored = Profile::new();
    let original_registry = original.registry();
    let restored_registry = restored.registry();
    let cancelled = AtomicBool::new(false);
    let bytes = fixture(2048, 1);
    let source = original.0.join("original.gguf");
    fs::write(&source, &bytes).unwrap();
    let mut model = original_registry
        .reference(&source, Purpose::Chat, &cancelled)
        .unwrap();
    model.name = "My restored model".into();
    model.cpu_threads = 3;
    let model = original_registry.configure(model).unwrap();
    let portable = portable_registration(&serde_json::to_vec(&model).unwrap(), &model.id).unwrap();
    let directory = restored_registry
        .root(true)
        .unwrap()
        .private_child(&model.id, true)
        .unwrap();
    directory.publish_copy(MANIFEST, &portable).unwrap();
    let detached = read_model(&directory, &model.id).unwrap();
    assert!(detached.external.is_none());
    assert!(restored_registry.open(&model.id).is_err());
    assert!(original_registry.check_reattachment(&model).is_err());
    assert!(restored_registry.check_reattachment(&detached).is_ok());

    let local = restored.0.join("other-machine.gguf");
    fs::write(&local, [bytes.as_slice(), &[0]].concat()).unwrap();
    assert!(
        restored_registry
            .reattach(&detached, &local, &cancelled)
            .is_err()
    );
    assert_eq!(read_model(&directory, &model.id).unwrap(), detached);
    fs::write(&local, &bytes).unwrap();
    directory.publish_copy(WEIGHTS, &bytes[..8]).unwrap();
    assert!(
        restored_registry
            .reattach(&detached, &local, &cancelled)
            .is_err()
    );
    directory.remove_file(WEIGHTS).unwrap();

    let mut stale = detached.clone();
    stale.cpu_threads += 1;
    assert!(
        restored_registry
            .reattach(&stale, &local, &cancelled)
            .is_err()
    );
    let attached = restored_registry
        .reattach(&detached, &local, &cancelled)
        .unwrap();
    let mut settings = attached.clone();
    settings.external = None;
    assert_eq!(settings, detached);
    let reopened = restored.registry().open(&model.id).unwrap();
    assert_eq!(reopened.model, attached);
    assert_eq!(reopened.path, local);
    reopened.revalidate().unwrap();
    drop(reopened);
    assert!(
        restored_registry
            .reattach(&detached, &local, &cancelled)
            .is_err()
    );
    fs::write(&local, [bytes.as_slice(), &[0]].concat()).unwrap();
    assert!(restored_registry.open(&model.id).is_err());
}

#[test]
fn restored_split_registration_requires_all_shards_with_the_original_sizes() {
    let profile = Profile::new();
    let registry = profile.registry();
    let cancelled = AtomicBool::new(false);
    let first = profile.0.join("split-00001-of-00002.gguf");
    let second = profile.0.join("split-00002-of-00002.gguf");
    let first_bytes = split::tests::fixture(0, 2, 2, false);
    let second_bytes = split::tests::fixture(1, 2, 2, false);
    fs::write(&first, &first_bytes).unwrap();
    fs::write(&second, &second_bytes).unwrap();
    let model = registry
        .reference(&second, Purpose::Chat, &cancelled)
        .unwrap();
    let root = registry.root(false).unwrap();
    let directory = root.private_child(&model.id, false).unwrap();
    let portable = portable_registration(&serde_json::to_vec(&model).unwrap(), &model.id).unwrap();
    directory.replace_file(MANIFEST, &portable).unwrap();
    let detached = read_model(&directory, &model.id).unwrap();

    fs::remove_file(&second).unwrap();
    assert!(registry.reattach(&detached, &first, &cancelled).is_err());
    fs::write(&second, [second_bytes.as_slice(), &[0]].concat()).unwrap();
    assert!(registry.reattach(&detached, &first, &cancelled).is_err());
    fs::write(&second, second_bytes).unwrap();
    let attached = registry.reattach(&detached, &second, &cancelled).unwrap();
    assert_eq!(attached.shard_bytes, detached.shard_bytes);
    let opened = registry.open(&model.id).unwrap();
    assert_eq!(opened.path, first);
    opened.revalidate().unwrap();
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
#[test]
#[ignore = "downloads a signed runtime and requires local GGUF weights and projector"]
fn native_gguf_vision_and_external_reference() {
    use std::time::{Duration, Instant};

    use crate::local_ai::runtime_package;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let asset = |name: &str| PathBuf::from(std::env::var_os(name).expect(name));
    let weights = asset("KORTTY_SMOKE_VISION_GGUF");
    let projector = asset("KORTTY_SMOKE_VISION_PROJECTOR");
    let private = crate::snippets::tests::Profile::new();
    let root = private.0.canonicalize().unwrap();
    let cancelled = AtomicBool::new(false);

    install_native_gguf_runtime(&root);
    let registry = Registry::new(root.clone());
    let pair = registry
        .import_projector_pair(&weights, &projector, &cancelled)
        .unwrap();
    let _stop_pair = StopNativeGguf(&root, &pair.id);
    assert!(pair.projector_bytes.is_some());
    let vision = native_gguf_client(&root, &pair.id);
    for (name, color) in [
        ("red", tiny_skia::Color::from_rgba8(255, 0, 0, 255)),
        ("blue", tiny_skia::Color::from_rgba8(0, 0, 255, 255)),
    ] {
        let mut image = tiny_skia::Pixmap::new(512, 512).unwrap();
        image.fill(color);
        let image = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(image.encode_png().unwrap())
        );
        let answer = vision
            .complete_vision_before_guarded(
                "Answer in one word.",
                "What is the main color of this image?",
                &image,
                false,
                Instant::now() + Duration::from_secs(120),
                || Ok(()),
            )
            .unwrap();
        assert!(
            answer
                .split(|ch: char| !ch.is_ascii_alphabetic())
                .any(|word| word.eq_ignore_ascii_case(name)),
            "{name}: {answer:?}"
        );
        println!("Private vision pair identified the {name} image: {answer}");
    }
    drop(vision);
    runtime_package::stop_model(&root, &pair.id).unwrap();

    let source = root.join("external-model.gguf");
    std::fs::copy(&weights, &source).unwrap();
    native_external_gguf_round_trip(&root, std::slice::from_ref(&source));
}

#[test]
#[ignore = "downloads a signed runtime and requires KORTTY_SMOKE_SPLIT_GGUF"]
fn native_split_gguf_external_reference() {
    let selected = PathBuf::from(std::env::var_os("KORTTY_SMOKE_SPLIT_GGUF").unwrap());
    let names = split::Names::parse(selected.file_name().unwrap().to_str().unwrap()).unwrap();
    let private = crate::snippets::tests::Profile::new();
    let root = private.0.canonicalize().unwrap();
    let files = (0..names.count)
        .map(|index| {
            let name = names.name(index);
            let path = root.join(&name);
            std::fs::copy(selected.with_file_name(name), &path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    install_native_gguf_runtime(&root);
    native_external_gguf_round_trip(&root, &files);
}

struct StopNativeGguf<'a>(&'a Path, &'a str);

impl Drop for StopNativeGguf<'_> {
    fn drop(&mut self) {
        let _ = crate::local_ai::runtime_package::stop_model(self.0, self.1);
    }
}

fn install_native_gguf_runtime(root: &Path) {
    use crate::local_ai::{runtime_index::PackageBackend, runtime_package::online};

    let backend = if cfg!(target_os = "macos") {
        PackageBackend::Metal
    } else {
        PackageBackend::Cpu
    };
    let pending = online::check(root.to_owned(), Some(backend)).unwrap();
    let catalog = pending.reply.recv_blocking().unwrap().unwrap();
    let choice = catalog.preferred_choice(Some(backend)).unwrap().clone();
    println!("Runtime {choice}");
    let download = online::install(root.to_owned(), catalog, choice).unwrap();
    assert!(matches!(
        download.pending.reply.recv_blocking().unwrap().unwrap(),
        online::InstallOutcome::Installed {
            cleanup_failed: false,
            ..
        }
    ));
}

fn native_gguf_client(root: &Path, model: &str) -> crate::local_ai::Client {
    use std::time::Duration;

    use crate::local_ai::{Client, Profile, ProfileOptions};

    Client::new(
        Profile::managed(
            root.to_owned(),
            model,
            ProfileOptions {
                max_output_tokens: Some(256),
                timeout: Some(Duration::from_secs(120)),
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

fn native_external_gguf_round_trip(root: &Path, paths: &[PathBuf]) {
    use crate::local_ai::{Client, runtime_package};

    let registry = Registry::new(root.to_owned());
    let cancelled = AtomicBool::new(false);
    // Register the last shard to verify that launch uses the first sibling.
    let source = paths.last().unwrap();
    let sizes = paths
        .iter()
        .map(|path| path.metadata().unwrap().len())
        .collect::<Vec<_>>();
    let external = registry
        .reference(source, Purpose::Chat, &cancelled)
        .unwrap();
    let _stop_external = StopNativeGguf(root, &external.id);
    assert_eq!(external.external.as_ref().unwrap().path(), paths[0]);
    if paths.len() > 1 {
        assert_eq!(external.shard_bytes, sizes);
    }
    let chat = native_gguf_client(root, &external.id);
    let ask = |chat: &Client| {
        chat.complete_chat("Give a short answer.", &[], "What color is a ripe banana?")
    };
    assert!(!ask(&chat).unwrap().trim().is_empty());
    let original = root.join("external-original.gguf");
    #[cfg(windows)]
    let chat = {
        let error = std::fs::rename(source, &original).unwrap_err();
        assert!(matches!(error.raw_os_error(), Some(5 | 32)), "{error}");
        println!("Windows prevented replacement while the read lease was active");
        drop(chat);
        runtime_package::stop_model(root, &external.id).unwrap();
        native_gguf_client(root, &external.id)
    };
    std::fs::rename(source, &original).unwrap();
    std::fs::copy(&original, source).unwrap();
    let failed = ask(&chat).unwrap_err().message();
    assert!(failed.contains("external GGUF changed"), "{failed}");
    drop(chat);
    runtime_package::stop_model(root, &external.id).unwrap();
    registry.remove(&external.id).unwrap();
    assert!(
        paths
            .iter()
            .zip(sizes)
            .all(|(path, size)| path.metadata().unwrap().len() == size)
    );
    let replacement = registry
        .reference(source, Purpose::Chat, &cancelled)
        .unwrap();
    let _stop_replacement = StopNativeGguf(root, &replacement.id);
    assert!(
        !ask(&native_gguf_client(root, &replacement.id))
            .unwrap()
            .trim()
            .is_empty()
    );
    println!(
        "External inference with {} file(s) passed, rejected a same-size replacement and resumed after re-registration",
        paths.len()
    );
}
