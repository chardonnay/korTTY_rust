use kortty_vault::{WebCredentialChange, WebProvider};
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::profile_backup::{inspect_java_password, prepare_java_credentials, queue_restore_at};

const SETTINGS: &str = include_str!("../../java_secrets/fixtures/java-web-settings.xml");
const JAVA_MASTER: &[u8] = include_bytes!("../../java_secrets/fixtures/java-web-master.properties");
const MASTER_PASSWORD: &str = "Owned Java provider master 2026";
const ARCHIVE_PASSWORD: &str = "Owned provider archive 2026";

fn archive(root: &Path, settings: &str, verifier: Option<&[u8]>) -> PathBuf {
    let path = root.join("java-web-settings.zip");
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, ARCHIVE_PASSWORD.as_bytes());
    zip.start_file("global-settings.xml", options).unwrap();
    zip.write_all(settings.as_bytes()).unwrap();
    if let Some(verifier) = verifier {
        zip.start_file("master.key", options).unwrap();
        zip.write_all(verifier).unwrap();
    }
    zip.finish().unwrap();
    path
}

#[test]
fn java_web_credentials_are_reviewed_sealed_and_additive() {
    let source = Scratch::new();
    let destination = Scratch::new();
    let path = archive(&source.0, SETTINGS, Some(JAVA_MASTER));
    let master = crate::vault_window::test_vault_password();
    let java_master = Secret::new(MASTER_PASSWORD.into());
    let archive_password = Secret::new(ARCHIVE_PASSWORD.into());
    let inspect = || inspect_java_password(&path, &archive_password).unwrap();
    let directory = Directory::open(&destination.0, false).unwrap();
    directory.private_child("vault", true).unwrap();
    let vault_path = crate::profile::vault_path_at(&destination.0);
    let mut vault = Vault::create(&vault_path, &master[..]).unwrap();
    let local = "fixture-local-bright-2026";
    vault
        .update_web_credentials(
            vec![(
                WebProvider::BrightData,
                WebCredentialChange::Set(Secret::new(local.into())),
            )],
            || Ok(()),
        )
        .unwrap();
    let original = fs::read(&vault_path).unwrap();
    let mut preview = inspect();
    assert!(preview.has_java_web_credentials());
    assert!(
        prepare_java_credentials(&mut preview, Some(&java_master), None, &destination.0).is_err()
    );
    assert!(
        prepare_java_credentials(
            &mut preview,
            Some(&archive_password),
            Some(&vault),
            &destination.0,
        )
        .is_err()
    );
    prepare_java_credentials(
        &mut preview,
        Some(&java_master),
        Some(&vault),
        &destination.0,
    )
    .unwrap();
    let summary = preview.java_web_credential_summary().unwrap();
    assert_eq!(summary.ready, [WebProvider::Tavily, WebProvider::Brave]);
    assert_eq!(summary.preserved, 1);
    assert_eq!(summary.omitted, 0);
    assert_eq!(fs::read(&vault_path).unwrap(), original);

    queue_restore_at(&destination.0, preview, RestoreMode::JavaAdditive).unwrap();
    let pending = fs::read(destination.0.join(PENDING)).unwrap();
    let ciphertext = roxmltree::Document::parse(SETTINGS).unwrap();
    for secret in [
        MASTER_PASSWORD,
        ARCHIVE_PASSWORD,
        local,
        "fixture-java-tavily-2026",
        "fixture-java-bright-2026",
        "fixture-java-brave-2026",
    ]
    .into_iter()
    .chain(
        ciphertext
            .root_element()
            .children()
            .filter_map(|field| field.is_element().then(|| field.text()).flatten()),
    ) {
        assert!(
            !pending
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        );
    }
    assert_eq!(fs::read(&vault_path).unwrap(), original);
    drop(vault);
    drop(ProfileLease::acquire(&destination.0, || false).unwrap());
    assert!(!directory.has_entry(PENDING).unwrap());
    let vault = Vault::unlock(&vault_path, &master[..]).unwrap();
    for (provider, expected) in [
        (WebProvider::Tavily, "fixture-java-tavily-2026"),
        (WebProvider::BrightData, local),
        (WebProvider::Brave, "fixture-java-brave-2026"),
    ] {
        assert_eq!(vault.web_credential(provider), Some(expected));
    }
    let installed = fs::read(&vault_path).unwrap();
    let mut replay = inspect();
    prepare_java_credentials(
        &mut replay,
        Some(&java_master),
        Some(&vault),
        &destination.0,
    )
    .unwrap();
    let summary = replay.java_web_credential_summary().unwrap();
    assert!(summary.ready.is_empty());
    assert_eq!(summary.preserved, 3);
    assert_eq!(summary.omitted, 0);
    assert!(replay.java_credentials.is_none());
    assert_eq!(fs::read(&vault_path).unwrap(), installed);
}

#[test]
fn java_web_credentials_report_ambiguous_and_tampered_fields() {
    let source = Scratch::new();
    let destination = Scratch::new();
    let settings = SETTINGS.replace(
        "</globalSettings>",
        "<encryptedAiTavilyApiKey>ambiguous</encryptedAiTavilyApiKey></globalSettings>",
    );
    let settings = settings.replace(
        "<encryptedAiBraveSearchApiKey>",
        "<encryptedAiBraveSearchApiKey>!",
    );
    let path = archive(&source.0, &settings, None);
    let archive_password = Secret::new(ARCHIVE_PASSWORD.into());
    let mut preview = inspect_java_password(&path, &archive_password).unwrap();
    assert!(
        preview.java_review().unwrap().2.iter().any(|notice| {
            notice.contains("Invalid Java web provider credential fields omitted")
        })
    );
    let vault = Vault::create(destination.0.join("vault.dat"), b"local master password").unwrap();
    let error = prepare_java_credentials(
        &mut preview,
        Some(&archive_password),
        Some(&vault),
        &destination.0,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let java_master = Secret::new(MASTER_PASSWORD.into());
    prepare_java_credentials(
        &mut preview,
        Some(&java_master),
        Some(&vault),
        &destination.0,
    )
    .unwrap();
    let summary = preview.java_web_credential_summary().unwrap();
    assert_eq!(summary.ready, [WebProvider::BrightData]);
    assert_eq!(summary.preserved, 0);
    assert_eq!(summary.omitted, 1);
}
