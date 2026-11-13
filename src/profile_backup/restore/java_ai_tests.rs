use std::path::PathBuf;

use kortty_vault::Secret;
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

use super::*;

const GLOBAL_SETTINGS: &[u8] =
    include_bytes!("../../ai_manager/profiles/fixtures/java-global-settings.xml");

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-java-https-restore-{}", nonce().unwrap()));
        Directory::open(&path, true).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn java_https_profile_backup_queues_and_replays_without_trust_or_default() {
    let scratch = Scratch::new();
    let path = scratch.0.join("java-ai-backup.zip");
    let password = Secret::new("java-archive-password".into());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    zip.start_file(
        "global-settings.xml",
        SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes()),
    )
    .unwrap();
    zip.write_all(GLOBAL_SETTINGS).unwrap();
    zip.finish().unwrap();

    let destination = scratch.0.join("profile");
    let live = destination.join(ai_backup::SETTINGS);
    let inspect = || super::super::inspect_java_password(&path, &password).unwrap();
    let preview = inspect();
    assert_eq!(preview.entries().collect::<Vec<_>>(), [ai_backup::SETTINGS]);
    let (_, imported, omitted) = preview.java_review().unwrap();
    assert!(
        imported
            .iter()
            .any(|line| line.contains("HTTPS AI profiles requiring destination review (2)"))
    );
    assert!(
        omitted
            .iter()
            .any(|line| line.contains("Java AI API keys omitted (1)"))
    );
    assert!(!live.exists());

    super::super::queue_restore_at(&destination, preview, RestoreMode::JavaAdditive).unwrap();
    assert!(!live.exists());
    let pending = fs::read(destination.join(PENDING)).unwrap();
    assert!(
        !pending
            .windows(b"synthetic-source-api-key".len())
            .any(|window| window == b"synthetic-source-api-key")
    );
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert!(!destination.join(PENDING).exists());
    let installed = fs::read(&live).unwrap();
    let settings: serde_json::Value = serde_json::from_slice(&installed).unwrap();
    assert!(settings["profiles"]["default"].is_null());
    let profiles = settings["profiles"]["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2);
    assert!(profiles.iter().all(|profile| {
        profile["restore_review"] == true
            && profile["trusted_local_endpoint"] == false
            && profile["trusted_remote_endpoint"] == false
            && profile["credential_env"] == ""
    }));
    assert_eq!(
        profiles[0]["endpoint"],
        "https://example.test/openai/v1/chat/completions"
    );

    super::super::queue_restore_at(&destination, inspect(), RestoreMode::JavaAdditive).unwrap();
    drop(ProfileLease::acquire(&destination, || false).unwrap());
    assert_eq!(fs::read(live).unwrap(), installed);
}
