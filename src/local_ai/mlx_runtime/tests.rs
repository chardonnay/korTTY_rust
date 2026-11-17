use std::io::{Cursor, Write as _};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer as _, SigningKey};
use pgp::crypto::hash::HashAlgorithm;
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

use super::*;

pub(super) fn archive(id: &str, launcher: bool, cpu: u32, extra: Option<&str>) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    let mut executable = [0_u8; 64];
    executable[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
    executable[4..8].copy_from_slice(&cpu.to_le_bytes());
    executable[12..16].copy_from_slice(&2_u32.to_le_bytes());
    zip.start_file(format!("{id}/{}", index::PYTHON), options)
        .unwrap();
    zip.write_all(&executable).unwrap();
    if launcher {
        zip.start_file(format!("{id}/{}", index::LAUNCHER), options)
            .unwrap();
        zip.write_all(b"fixture Python launcher\n").unwrap();
    }
    if let Some(extra) = extra {
        zip.start_file(extra, options).unwrap();
        zip.write_all(b"invalid extra path").unwrap();
    }
    zip.finish().unwrap().into_inner()
}

pub(super) fn descriptor(id: &str, archive: &[u8]) -> Value {
    let sha256: String = HashAlgorithm::Sha256
        .digest(archive)
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    json!({
        "schemaVersion": 1,
        "runtimeId": id.trim_end_matches("-macos-aarch64"),
        "installationId": id,
        "platform": "macos",
        "architecture": "aarch64",
        "backend": "MLX",
        "minimumOsVersion": "14.2",
        "mlxLmVersion": "0.32.2",
        "pythonVersion": "3.12.13",
        "sourceCommit": "1ebbe724ee84",
        "executablePath": index::PYTHON,
        "launcherPath": index::LAUNCHER,
        "sizeBytes": archive.len(),
        "sha256": sha256,
        "requirementsLockSha256": "b".repeat(64),
        "downloadUrl": "https://example.invalid/fixture.zip",
    })
}

pub(super) fn write_catalog(
    directory: &Path,
    stem: &str,
    day: u8,
    packages: &[Value],
    revoked: &[&str],
    key: &SigningKey,
) -> (PathBuf, PathBuf) {
    let bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1, "generatedAt": format!("2026-11-{day:02}T00:00:00Z"),
        "packages": packages, "revokedRuntimeIds": revoked,
    }))
    .unwrap();
    let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
    let index = directory.join(format!("{stem}.json"));
    let signed = directory.join(format!("{stem}.sig"));
    std::fs::write(&index, bytes).unwrap();
    std::fs::write(&signed, signature).unwrap();
    (index, signed)
}

fn inputs(directory: &Path, id: &str, day: u8, archive: &[u8], key: &SigningKey) -> Import {
    let (index, signature) =
        write_catalog(directory, id, day, &[descriptor(id, archive)], &[], key);
    let path = directory.join(format!("{id}.zip"));
    std::fs::write(&path, archive).unwrap();
    Import {
        index,
        signature,
        archive: path,
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "downloads a signed MLX release and requires KORTTY_SMOKE_MLX_MODEL"]
fn native_signed_mlx_qwen_reasoning_round_trip() {
    native_signed_mlx_reasoning(
        "KORTTY_SMOKE_MLX_MODEL",
        crate::local_ai::mlx_models::ReasoningControls::QwenThinking,
        &[("none", false), ("on", true)],
        "low",
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "downloads a signed MLX release and requires KORTTY_SMOKE_MLX_GPT_OSS_MODEL"]
fn native_signed_mlx_gpt_oss_reasoning_round_trip() {
    native_signed_mlx_reasoning(
        "KORTTY_SMOKE_MLX_GPT_OSS_MODEL",
        crate::local_ai::mlx_models::ReasoningControls::GptOssEffort,
        &[("low", true), ("medium", true), ("high", true)],
        "on",
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn install_native_runtime(root: &Path, expected_id: Option<&str>) -> String {
    let pending = online::check(root.to_owned()).unwrap();
    let catalog = pending.reply.recv_blocking().unwrap().unwrap();
    let choice = catalog
        .choices()
        .iter()
        .find(|choice| expected_id.is_none_or(|id| choice.id == id))
        .expect("Compatible signed runtime")
        .clone();
    println!("Runtime {choice}");
    let pending = online::download(root.to_owned(), catalog, choice.clone()).unwrap();
    let installed = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(installed.activated);
    assert_eq!(
        installed.runtime.active_installation_id(),
        Some(choice.id.as_str())
    );
    assert!(installed.runtime.pending_first_launch());
    choice.id
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct StopModel<'a>(&'a Path, &'a str);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for StopModel<'_> {
    fn drop(&mut self) {
        let _ = crate::local_ai::runtime_package::with_stopped_mlx_model(self.0, self.1, || Ok(()));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "downloads a signed MLX release and requires KORTTY_SMOKE_MLX_MODEL"]
fn native_signed_mlx_lifecycle_round_trip() {
    use std::time::{Duration, Instant};

    use crate::local_ai::runtime_package::{ModelStatus, mlx_model_status, with_stopped_mlx_model};
    use crate::local_ai::{Client, Error, Profile, ProfileOptions, StreamDelta, mlx_models};

    let source = PathBuf::from(std::env::var_os("KORTTY_SMOKE_MLX_MODEL").unwrap());
    let private_profile = crate::snippets::tests::Profile::new();
    let root = private_profile.0.canonicalize().unwrap();
    let runtime = install_native_runtime(&root, None);
    let cancelled = AtomicBool::new(false);
    let registry = mlx_models::Registry::new(&root);
    let model = registry
        .import(
            &mlx_models::Review::prepare(&source, &cancelled).unwrap(),
            &cancelled,
        )
        .unwrap();
    let model = registry.configure(&model.id, &model.name, 1).unwrap();
    let stop = StopModel(&root, &model.id);
    let status = || mlx_model_status(&root, &model.id);
    let profile = Profile::managed_mlx(
        root.clone(),
        &model.id,
        ProfileOptions {
            reasoning_effort: Some("none".into()),
            max_output_tokens: Some(512),
            timeout: Some(Duration::from_secs(120)),
            ..Default::default()
        },
    )
    .unwrap();

    let (client, cancel) = Client::new_cancellable(profile.clone());
    let mut cancel = Some(cancel);
    let result = client.stream_chat(
        "Answer in plain text.",
        &[],
        "Name the letters of the alphabet one at a time.",
        || Ok(()),
        |delta| {
            if matches!(delta, StreamDelta::Text(_)) {
                drop(cancel.take());
            }
        },
    );
    assert!(cancel.is_none(), "The active stream did not emit text");
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert_eq!(status(), ModelStatus::Busy);
    let plan = review_removal(&root, &cancelled).unwrap().unwrap();
    let error = remove(&root, &plan, &cancelled).unwrap_err();
    assert!(error.contains("handling a request"), "{error}");
    assert_eq!(
        self::installed(&root).unwrap().active_installation_id(),
        Some(runtime.as_str())
    );
    println!("Cancelled an active stream and refused removal while its lease was held");
    let idle_since = Instant::now();
    drop(client);
    assert_eq!(status(), ModelStatus::Ready);
    while status() != ModelStatus::Stopped {
        assert_ne!(status(), ModelStatus::Failed);
        assert!(
            idle_since.elapsed() < Duration::from_secs(90),
            "Sidecar status after idle timeout: {:?}",
            status()
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(idle_since.elapsed() >= Duration::from_secs(60));
    println!(
        "Idle sidecar stopped after {} seconds",
        idle_since.elapsed().as_secs()
    );

    let answer = || {
        Client::new(profile.clone())
            .complete_chat("Answer in one word.", &[], "What is the capital of France?")
            .unwrap()
    };
    assert_answer_is_paris(&answer());
    assert_eq!(status(), ModelStatus::Ready);
    println!("The next request restarted the idle sidecar");
    let plan = review_removal(&root, &cancelled).unwrap().unwrap();
    assert_eq!(plan.package_count(), 1);
    assert_eq!(remove(&root, &plan, &cancelled).unwrap(), None);
    assert_eq!(status(), ModelStatus::Stopped);
    assert!(
        self::installed(&root)
            .unwrap()
            .active_installation_id()
            .is_none()
    );
    assert!(review_removal(&root, &cancelled).unwrap().is_none());
    assert_eq!(registry.list().unwrap(), vec![model.clone()]);
    println!("Removed the runtime with the model registration intact");

    assert_eq!(install_native_runtime(&root, Some(&runtime)), runtime);
    assert_answer_is_paris(&answer());
    assert!(!self::installed(&root).unwrap().pending_first_launch());
    with_stopped_mlx_model(&root, &model.id, || registry.remove(&model.id)).unwrap();
    drop(stop);
    assert!(registry.list().unwrap().is_empty());
    println!("Reinstalled and started the same signed release with the preserved model");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "downloads two signed MLX releases and requires KORTTY_SMOKE_MLX_MODEL"]
fn native_signed_mlx_update_and_rollback() {
    native_signed_mlx_recovery(UpdateFailure::DamagedInstallation);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
#[ignore = "downloads two signed MLX releases and requires KORTTY_SMOKE_MLX_MODEL"]
fn native_signed_mlx_process_exit_and_rollback() {
    native_signed_mlx_recovery(UpdateFailure::ProcessExit);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
enum UpdateFailure {
    DamagedInstallation,
    ProcessExit,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn native_signed_mlx_recovery(failure: UpdateFailure) {
    use std::time::Duration;

    use crate::local_ai::{Client, Error, Profile, ProfileOptions, mlx_models};

    const OLDER: &str = "mlx-0.31.3-kortty1-macos-aarch64";
    const NEWER: &str = "mlx-0.31.3-kortty2-macos-aarch64";

    let source = PathBuf::from(std::env::var_os("KORTTY_SMOKE_MLX_MODEL").unwrap());
    let private_profile = crate::snippets::tests::Profile::new();
    let root = private_profile.0.canonicalize().unwrap();
    let cancelled = AtomicBool::new(false);
    let registry = mlx_models::Registry::new(&root);
    let review = mlx_models::Review::prepare(&source, &cancelled).unwrap();
    let model = registry.import(&review, &cancelled).unwrap();
    let _stop = StopModel(&root, &model.id);

    let complete = || {
        let profile = Profile::managed_mlx(
            root.clone(),
            &model.id,
            ProfileOptions {
                reasoning_effort: Some("none".into()),
                max_output_tokens: Some(512),
                timeout: Some(Duration::from_secs(120)),
                ..Default::default()
            },
        )
        .unwrap();
        Client::new(profile).complete_chat(
            "Answer in one word.",
            &[],
            "What is the capital of France?",
        )
    };
    assert_eq!(install_native_runtime(&root, Some(OLDER)), OLDER);
    assert_answer_is_paris(&complete().unwrap());
    let state = self::installed(&root).unwrap();
    assert!(!state.pending_first_launch());
    let pin = state.pinned_runtime_id().expect("Older release was pinned");
    assert_eq!(Some(pin), state.active_runtime_id());
    let state = unpin(&root, pin, &cancelled).unwrap();
    assert_eq!(state.active_installation_id(), Some(OLDER));
    assert!(state.pinned_runtime_id().is_none());
    println!("Started the older signed release and cleared its pin");

    assert_eq!(install_native_runtime(&root, Some(NEWER)), NEWER);
    match failure {
        UpdateFailure::DamagedInstallation => {
            let pending = Runtime::open(&root, &cancelled).unwrap();
            let launcher = pending.launcher().to_owned();
            assert!(launcher.starts_with(root.join("mlx-runtime")));
            drop(pending);
            std::fs::remove_file(launcher).unwrap();
            let failed = complete();
            assert!(matches!(failed, Err(Error::Runtime(_))), "{failed:?}");
        }
        UpdateFailure::ProcessExit => {
            let failed = crate::local_ai::runtime_package::test_mlx_process_exit_before_ready(
                &root, &model.id, &cancelled,
            );
            let message = failed.to_string();
            assert!(
                message.contains("managed model process exited"),
                "{message}"
            );
            assert!(
                message.contains("Restored the previous verified MLX runtime"),
                "{message}"
            );
        }
    }
    let state = self::installed(&root).unwrap();
    assert_eq!(state.active_installation_id(), Some(OLDER));
    assert!(!state.pending_first_launch());
    assert!(state.pinned_runtime_id().is_none());
    assert_answer_is_paris(&complete().unwrap());
    println!("Recovered the older verified runtime after {failure:?}");

    assert_eq!(install_native_runtime(&root, Some(NEWER)), NEWER);
    assert_answer_is_paris(&complete().unwrap());
    let state = self::installed(&root).unwrap();
    assert_eq!(state.active_installation_id(), Some(NEWER));
    assert!(!state.pending_first_launch());
    assert!(state.pinned_runtime_id().is_none());
    println!("Retried the update and completed inference with the newer signed release");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn native_signed_mlx_reasoning(
    source_variable: &str,
    controls: crate::local_ai::mlx_models::ReasoningControls,
    settings: &[(&str, bool)],
    stale_setting: &str,
) {
    use std::time::Duration;

    use crate::local_ai::runtime_package::with_stopped_mlx_model;
    use crate::local_ai::{Client, Error, Profile, ProfileOptions, StreamDelta, mlx_models};

    let source = PathBuf::from(std::env::var_os(source_variable).expect(source_variable));
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let cancelled = AtomicBool::new(false);
    install_native_runtime(&root, None);
    let registry = mlx_models::Registry::new(&root);
    let review = mlx_models::Review::prepare(&source, &cancelled).unwrap();
    let model = registry.import(&review, &cancelled).unwrap();
    let opened = registry.open(&model.id).unwrap();
    assert_eq!(opened.reasoning_controls(), controls);
    drop(opened);

    let stop = StopModel(&root, &model.id);
    let client = |setting: Option<&str>| {
        let route = Profile::managed_mlx(
            root.clone(),
            &model.id,
            ProfileOptions {
                reasoning_effort: setting.map(str::to_owned),
                max_output_tokens: Some(4096),
                timeout: Some(Duration::from_secs(300)),
                ..Default::default()
            },
        )
        .unwrap();
        Client::new(route)
    };
    for &(setting, expects_reasoning) in settings {
        let mut reasoning = String::new();
        let answer = client(Some(setting))
            .stream_chat(
                "Answer in one word.",
                &[],
                "What is the capital of France?",
                || Ok(()),
                |delta| {
                    if let StreamDelta::Reasoning(part) = delta {
                        reasoning.push_str(&part);
                    }
                },
            )
            .unwrap();
        assert!(!answer.trim().is_empty(), "Empty answer for {setting}");
        if controls == mlx_models::ReasoningControls::GptOssEffort {
            assert_answer_is_paris(&answer);
        }
        assert_eq!(
            !reasoning.trim().is_empty(),
            expects_reasoning,
            "Reasoning for {setting}, answer prefix: {:?}",
            answer.chars().take(256).collect::<String>()
        );
        println!(
            "{controls:?} setting={setting} answer_bytes={} reasoning_bytes={}",
            answer.len(),
            reasoning.len()
        );
    }
    assert!(!self::installed(&root).unwrap().pending_first_launch());
    if controls == mlx_models::ReasoningControls::GptOssEffort {
        let answer = client(None)
            .complete_chat("Answer in one word.", &[], "What is the capital of France?")
            .unwrap();
        assert_answer_is_paris(&answer);
        println!(
            "{controls:?} default buffered answer_bytes={}",
            answer.len()
        );
    }
    let stale = client(Some(stale_setting)).stream_chat(
        "Answer in one word.",
        &[],
        "What is the capital of France?",
        || Ok(()),
        |_| panic!("Unsupported reasoning setting yielded a response delta"),
    );
    assert!(
        matches!(
            stale,
            Err(Error::Invalid(mlx_models::UNSUPPORTED_REASONING))
        ),
        "{stale:?}"
    );
    println!("{controls:?} rejected stale setting={stale_setting}");
    with_stopped_mlx_model(&root, &model.id, || registry.remove(&model.id)).unwrap();
    drop(stop);
    assert!(registry.list().unwrap().is_empty());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_answer_is_paris(answer: &str) {
    assert!(
        answer
            .trim()
            .trim_end_matches('.')
            .eq_ignore_ascii_case("Paris"),
        "Unexpected answer: {answer:?}"
    );
}

#[test]
fn mlx_signed_local_install_locks_revalidates_and_recovers_the_last_ready_package() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = key.verifying_key();
    let host = OsVersion::parse("14.5.1").unwrap();
    let cancelled = AtomicBool::new(false);
    let first = "mlx-0.32.2-kortty1-macos-aarch64";
    let second = "mlx-0.32.2-kortty2-macos-aarch64";
    let first_zip = archive(first, true, 0x0100_000c, None);
    let first_review = review_on(
        inputs(&root, first, 2, &first_zip, &key),
        host,
        &public,
        &cancelled,
    )
    .unwrap();
    assert_eq!(first_review.installation_id(), first);
    assert_eq!(first_review.archive_bytes(), first_zip.len() as u64);
    assert_eq!(first_review.mlx_version(), "0.32.2");
    assert_eq!(first_review.archive_sha256().len(), 64);
    assert_eq!(first_review.index_sha256().len(), 64);
    assert_eq!(
        store::install(&root, &first_review, host, &public, &cancelled).unwrap(),
        first
    );
    let mut runtime = Runtime::open_on(&root, host, &public, &cancelled).unwrap();
    assert_eq!(runtime.executable(), runtime.payload().join(index::PYTHON));
    assert_eq!(runtime.launcher(), runtime.payload().join(index::LAUNCHER));
    runtime.ready().unwrap();
    runtime.revalidate().unwrap();

    let token = runtime.key_file("one-use-fixture-token-123").unwrap();
    let session = token.path().parent().unwrap().to_owned();
    assert!(session.join("huggingface/hub").is_dir());
    assert_eq!(
        std::fs::read_to_string(token.path()).unwrap(),
        "one-use-fixture-token-123"
    );
    token.clear_token().unwrap();
    assert!(session.is_dir());
    assert!(!token.path().exists());
    drop(token);
    assert!(!session.exists());

    let second_zip = archive(second, true, 0x0100_000c, None);
    let second_review = review_on(
        inputs(&root, second, 3, &second_zip, &key),
        host,
        &public,
        &cancelled,
    )
    .unwrap();
    assert!(store::install(&root, &second_review, host, &public, &cancelled).is_err());
    drop(runtime);
    assert_eq!(
        store::install(&root, &second_review, host, &public, &cancelled).unwrap(),
        second
    );
    let mut pending = Runtime::open_on(&root, host, &public, &cancelled).unwrap();
    assert!(pending.recover(&cancelled).unwrap());
    drop(pending);
    assert_eq!(store::active(&root).unwrap().as_deref(), Some(first));
    Runtime::open_on(&root, host, &public, &cancelled)
        .unwrap()
        .revalidate()
        .unwrap();
    assert!(!root.join("llama-runtime").exists());
}

#[test]
fn mlx_catalog_only_withdrawal_is_persistent_and_cannot_be_replayed() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let key = SigningKey::from_bytes(&[8; 32]);
    let public = key.verifying_key();
    let host = OsVersion::parse("15.1").unwrap();
    let cancelled = AtomicBool::new(false);
    let id = "mlx-0.32.2-kortty1-macos-aarch64";
    let zip = archive(id, true, 0x0100_000c, None);
    let inputs = inputs(&root, id, 2, &zip, &key);
    let approved = review_on(inputs.clone(), host, &public, &cancelled).unwrap();
    store::install(&root, &approved, host, &public, &cancelled).unwrap();
    Runtime::open_on(&root, host, &public, &cancelled)
        .unwrap()
        .ready()
        .unwrap();
    let (index, signature) = write_catalog(&root, "withdrawn", 3, &[], &[id], &key);
    let (review, _) = catalog_review(index, signature, &public).unwrap();
    assert_eq!(review.revoked_count(), 1);
    store::accept_catalog(&root, &review, host, &public, &cancelled).unwrap();
    assert!(store::active(&root).unwrap().is_none());
    assert!(Runtime::open_on(&root, host, &public, &cancelled).is_err());
    assert!(store::install(&root, &approved, host, &public, &cancelled).is_err());

    let (index, signature) =
        write_catalog(&root, "same-date", 3, &[descriptor(id, &zip)], &[], &key);
    let (review, _) = catalog_review(index, signature, &public).unwrap();
    assert!(store::accept_catalog(&root, &review, host, &public, &cancelled).is_err());
    let (index, signature) = write_catalog(&root, "later", 4, &[descriptor(id, &zip)], &[], &key);
    let later = review_on(
        Import {
            index,
            signature,
            archive: inputs.archive,
        },
        host,
        &public,
        &cancelled,
    )
    .unwrap();
    assert!(store::install(&root, &later, host, &public, &cancelled).is_err());
}

#[test]
fn mlx_runtime_removal_keeps_registered_models_and_signed_trust() {
    use crate::local_ai::mlx_models;

    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let key = SigningKey::from_bytes(&[9; 32]);
    let public = key.verifying_key();
    let host = OsVersion::parse("15.1").unwrap();
    let cancelled = AtomicBool::new(false);
    let first = "mlx-0.32.2-kortty1-macos-aarch64";
    let second = "mlx-0.32.2-kortty2-macos-aarch64";
    let revoked = "mlx-0.32.2-kortty3-macos-aarch64";
    let first_zip = archive(first, true, 0x0100_000c, None);
    let approved_first = review_on(
        inputs(&root, first, 2, &first_zip, &key),
        host,
        &public,
        &cancelled,
    )
    .unwrap();
    assert!(store::review_removal(&root, &cancelled).unwrap().is_none());
    store::install(&root, &approved_first, host, &public, &cancelled).unwrap();
    Runtime::open_on(&root, host, &public, &cancelled)
        .unwrap()
        .ready()
        .unwrap();
    let old = store::review_removal(&root, &cancelled).unwrap().unwrap();

    let second_zip = archive(second, true, 0x0100_000c, None);
    let mut paths = inputs(&root, second, 3, &second_zip, &key);
    (paths.index, paths.signature) = write_catalog(
        &root,
        "new",
        3,
        &[descriptor(second, &second_zip)],
        &[revoked],
        &key,
    );
    let approved_second = review_on(paths, host, &public, &cancelled).unwrap();
    store::install(&root, &approved_second, host, &public, &cancelled).unwrap();
    let mut lease = Runtime::open_on(&root, host, &public, &cancelled).unwrap();
    lease.ready().unwrap();
    let plan = store::review_removal(&root, &cancelled).unwrap().unwrap();
    assert_eq!(plan.package_count(), 2);
    let pointer = root.join("mlx-runtime/activation.json");
    let before: Value = serde_json::from_slice(&std::fs::read(&pointer).unwrap()).unwrap();
    assert!(
        store::uninstall(&root, &old, &cancelled)
            .unwrap_err()
            .to_string()
            .contains("Review removal again")
    );
    assert!(
        store::uninstall(&root, &plan, &cancelled)
            .unwrap_err()
            .to_string()
            .contains("Finish active MLX model requests")
    );
    assert_eq!(store::active(&root).unwrap().as_deref(), Some(second));
    assert_eq!(
        store::review_removal(&root, &cancelled).unwrap(),
        Some(plan)
    );
    drop(lease);

    let source = root.join("Model fixture");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("config.json"), br#"{"model_type":"llama"}"#).unwrap();
    std::fs::write(source.join("tokenizer.json"), b"{}").unwrap();
    let header = br#"{"x":{"dtype":"U8","shape":[1],"data_offsets":[0,1]}}"#;
    let mut weights = (header.len() as u64).to_le_bytes().to_vec();
    weights.extend_from_slice(header);
    weights.push(1);
    std::fs::write(source.join("model.safetensors"), &weights).unwrap();
    let model_review = mlx_models::Review::prepare(&source, &cancelled).unwrap();
    let registry = mlx_models::Registry::new(&root);
    let model = registry.import(&model_review, &cancelled).unwrap();

    let plan = store::review_removal(&root, &cancelled).unwrap().unwrap();
    assert_eq!(
        store::uninstall(&root, &plan, &AtomicBool::new(true))
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(store::uninstall(&root, &plan, &cancelled).unwrap(), None);
    assert!(store::active(&root).unwrap().is_none());
    assert!(store::review_removal(&root, &cancelled).unwrap().is_none());
    assert_eq!(
        std::fs::read_dir(root.join("mlx-runtime/installations"))
            .unwrap()
            .count(),
        0
    );
    let after: Value = serde_json::from_slice(&std::fs::read(pointer).unwrap()).unwrap();
    assert_eq!(after["accepted"], before["accepted"]);
    assert_eq!(after["accepted"]["revoked"], json!([revoked]));
    assert_eq!(after["active"], Value::Null);
    assert_eq!(after["previous"], Value::Null);
    assert_eq!(after["pending"], json!(false));
    assert!(
        store::install(&root, &approved_first, host, &public, &cancelled)
            .unwrap_err()
            .to_string()
            .contains("older than or conflicts with the accepted catalog")
    );
    let registered = registry.open(&model.id).unwrap();
    registered.revalidate().unwrap();
    assert_eq!(
        std::fs::read(registered.path.join("model.safetensors")).unwrap(),
        weights
    );
}

#[test]
fn mlx_review_rejects_wrong_signature_os_or_changed_source() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let key = SigningKey::from_bytes(&[9; 32]);
    let public = key.verifying_key();
    let other = SigningKey::from_bytes(&[10; 32]).verifying_key();
    let host = OsVersion::parse("14.2").unwrap();
    let cancelled = AtomicBool::new(false);
    let id = "mlx-0.32.2-kortty1-macos-aarch64";
    let zip = archive(id, true, 0x0100_000c, None);
    let inputs = inputs(&root, id, 2, &zip, &key);
    assert!(review_on(inputs.clone(), host, &other, &cancelled).is_err());
    assert!(review_on(inputs.clone(), OsVersion::MINIMUM, &public, &cancelled).is_err());
    let review = review_on(inputs.clone(), host, &public, &cancelled).unwrap();
    std::fs::write(&inputs.archive, b"replacement ZIP").unwrap();
    assert!(store::install(&root, &review, host, &public, &cancelled).is_err());
    assert!(!root.join("mlx-runtime").exists());
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        review_on(inputs, host, &public, &cancelled)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn mlx_signed_zip_rejects_missing_launcher_wrong_architecture_and_foreign_wrapper() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let key = SigningKey::from_bytes(&[11; 32]);
    let public = key.verifying_key();
    let host = OsVersion::parse("14.2").unwrap();
    let cancelled = AtomicBool::new(false);
    let id = "mlx-0.32.2-kortty1-macos-aarch64";
    for zip in [
        archive(id, false, 0x0100_000c, None),
        archive(id, true, 0x0100_0007, None),
        archive(id, true, 0x0100_000c, Some("foreign/code.py")),
    ] {
        let review =
            review_on(inputs(&root, id, 2, &zip, &key), host, &public, &cancelled).unwrap();
        assert!(store::install(&root, &review, host, &public, &cancelled).is_err());
        assert!(store::active(&root).unwrap().is_none());
        assert_eq!(
            std::fs::read_dir(root.join("mlx-runtime/installations"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn mlx_public_entrypoints_require_an_apple_silicon_host_before_opening_inputs() {
    let paths = Import {
        index: PathBuf::new(),
        signature: PathBuf::new(),
        archive: PathBuf::new(),
    };
    let cancelled = AtomicBool::new(false);
    assert!(!supported());
    assert_eq!(
        review(paths, &cancelled).err().unwrap(),
        "MLX requires Apple Silicon and macOS 14 or later"
    );
    assert!(
        installed(Path::new(""))
            .unwrap()
            .active_installation_id()
            .is_none()
    );
    assert_eq!(
        review_removal(Path::new(""), &cancelled).unwrap_err(),
        "MLX requires Apple Silicon and macOS 14 or later"
    );
    assert_eq!(
        online::check(PathBuf::new()).err().unwrap(),
        "MLX requires Apple Silicon and macOS 14 or later"
    );
}
