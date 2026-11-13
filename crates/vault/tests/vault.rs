use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;

use kortty_vault::{
    CredentialAddition, CredentialInput, CredentialKind, Error, MAX_QDRANT_API_KEY_BYTES,
    MAX_RECOVERY_OUTPUT_BYTES, PreparedState, Secret, Vault, WebhookFormat, WebhookSettings,
    valid_credential_id,
};

const MASTER: &[u8] = b"test master password only";
const CHANGED: &[u8] = b"new test master password only";
const WRONG: &[u8] = b"wrong test master password only";
const TEST_SECRET: &str = "test-only ssh secret ä";

#[test]
fn prepared_additions_are_sealed_without_changing_the_live_vault() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let existing = vault
        .save(
            CredentialInput {
                label: "Existing SSH password".into(),
                ..Default::default()
            },
            Secret::new(TEST_SECRET.into()),
        )
        .unwrap();
    let before = fs::read(&path).unwrap();
    let webhook = WebhookSettings {
        enabled: false,
        format: WebhookFormat::Slack,
        include_summary: true,
    };
    let prepared = vault
        .prepare_additions(vec![
            password_addition(),
            CredentialAddition {
                input: CredentialInput {
                    label: "Imported webhook".into(),
                    kind: CredentialKind::JobWebhookUrl,
                    ..Default::default()
                },
                secret: Secret::new("https://example.test/private-hook".into()),
                webhook: Some(webhook),
            },
        ])
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(vault.entries().count(), 1);
    let imported = b"imported test password";
    assert!(
        !prepared
            .snapshot
            .windows(imported.len())
            .any(|window| window == imported)
    );
    let [password_id, webhook_id] = prepared.ids.as_slice() else {
        panic!("expected two new credential IDs");
    };
    assert!(valid_credential_id(password_id));
    assert!(valid_credential_id(webhook_id));
    assert_ne!(password_id, webhook_id);
    assert_ne!(password_id, &existing);
    assert_ne!(webhook_id, &existing);
    let verify = || {
        Vault::verify_prepared_additions(
            &path,
            &prepared.before,
            &prepared.after,
            &prepared.snapshot,
        )
    };
    let apply = || {
        Vault::apply_prepared_additions(
            &path,
            &prepared.before,
            &prepared.after,
            &prepared.snapshot,
        )
    };
    assert!(matches!(verify(), Err(Error::Busy)));
    assert!(matches!(apply(), Err(Error::Busy)));
    assert_eq!(fs::read(&path).unwrap(), before);
    vault.lock();
    assert_eq!(verify().unwrap(), PreparedState::Before);

    assert_eq!(apply().unwrap(), PreparedState::Before);
    assert_eq!(fs::read(&path).unwrap(), *prepared.snapshot);
    assert_eq!(verify().unwrap(), PreparedState::After);
    assert_eq!(apply().unwrap(), PreparedState::After);
    assert_eq!(fs::read(&path).unwrap(), *prepared.snapshot);
    let reopened = Vault::unlock(&path, MASTER).unwrap();
    assert_eq!(reopened.secret(&existing), Some(TEST_SECRET));
    assert_eq!(reopened.secret(password_id), Some("imported test password"));
    assert_eq!(
        reopened.get(webhook_id).unwrap().webhook_settings(),
        Some(webhook)
    );
    assert_eq!(
        reopened.job_webhook_url(webhook_id),
        Some("https://example.test/private-hook")
    );
}

#[test]
fn prepared_additions_reject_invalid_or_stale_input_without_writing() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        vault.prepare_additions(Vec::new()),
        Err(Error::InvalidInput(_))
    ));
    let mut update = password_addition();
    update.input.id = Some("a".repeat(32));
    assert!(matches!(
        vault.prepare_additions(vec![password_addition(), update]),
        Err(Error::InvalidInput(_))
    ));
    let mut wrong_kind = password_addition();
    wrong_kind.webhook = Some(WebhookSettings::default());
    assert!(matches!(
        vault.prepare_additions(vec![wrong_kind]),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        vault.prepare_additions((0..1025).map(|_| password_addition()).collect()),
        Err(Error::Limit)
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(vault.entries().count(), 0);

    let prepared = vault.prepare_additions(vec![password_addition()]).unwrap();
    let added = password_addition();
    vault.save(added.input, added.secret).unwrap();
    vault.lock();
    let current = fs::read(&path).unwrap();
    assert!(matches!(
        Vault::verify_prepared_additions(
            &path,
            &prepared.before,
            &prepared.after,
            &prepared.snapshot
        ),
        Err(Error::Changed)
    ));
    assert!(matches!(
        Vault::apply_prepared_additions(
            &path,
            &prepared.before,
            &prepared.after,
            &prepared.snapshot
        ),
        Err(Error::Changed)
    ));
    let mut tampered = prepared.snapshot.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(matches!(
        Vault::verify_prepared_additions(&path, &prepared.before, &prepared.after, &tampered),
        Err(Error::InvalidFormat)
    ));
    assert!(matches!(
        Vault::apply_prepared_additions(&path, &prepared.before, &prepared.after, &tampered),
        Err(Error::InvalidFormat)
    ));
    let missing = directory.0.join("missing");
    assert!(matches!(
        Vault::apply_prepared_additions(&missing, &prepared.before, &prepared.after, &tampered),
        Err(Error::InvalidFormat)
    ));
    assert!(matches!(
        Vault::apply_prepared_additions(
            &missing,
            &prepared.before,
            &prepared.after,
            &prepared.snapshot
        ),
        Err(Error::NotFound)
    ));
    assert!(!directory.0.join("missing.lock").exists());
    assert_eq!(fs::read(&path).unwrap(), current);
}

fn password_addition() -> CredentialAddition {
    CredentialAddition {
        input: CredentialInput {
            label: "Imported SSH password".into(),
            ..Default::default()
        },
        secret: Secret::new("imported test password".into()),
        webhook: None,
    }
}

#[test]
fn recovery_output_binds_the_layout_and_vault_key() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let vault = Vault::create(&path, MASTER).unwrap();
    let layout = b"session layout A";
    let output = b"private terminal output";
    let sealed = vault.seal_recovery_output(layout, output).unwrap();
    assert_ne!(sealed, vault.seal_recovery_output(layout, output).unwrap());
    assert!(!sealed.windows(output.len()).any(|bytes| bytes == output));
    assert_eq!(
        vault
            .open_recovery_output(layout, &sealed)
            .unwrap()
            .as_slice(),
        output
    );
    assert!(matches!(
        vault.open_recovery_output(b"session layout B", &sealed),
        Err(Error::Authentication)
    ));
    let mut tampered = sealed.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(matches!(
        vault.open_recovery_output(layout, &tampered),
        Err(Error::Authentication)
    ));
    assert!(matches!(
        vault.seal_recovery_output(layout, &vec![0; MAX_RECOVERY_OUTPUT_BYTES + 1]),
        Err(Error::Limit)
    ));

    vault.lock();
    let mut vault = Vault::unlock(&path, MASTER).unwrap();
    assert_eq!(
        vault
            .open_recovery_output(layout, &sealed)
            .unwrap()
            .as_slice(),
        output
    );
    vault.change_master_password(MASTER, CHANGED).unwrap();
    assert!(matches!(
        vault.open_recovery_output(layout, &sealed),
        Err(Error::Authentication)
    ));
}

#[test]
fn qdrant_api_key_validates_and_keeps_its_type_after_reopen() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let ssh = vault
        .save(
            CredentialInput {
                label: "Existing SSH password".into(),
                ..Default::default()
            },
            Secret::new(TEST_SECRET.into()),
        )
        .unwrap();
    let input = CredentialInput {
        label: "Fixture Qdrant key".into(),
        kind: CredentialKind::QdrantApiKey,
        ..Default::default()
    };
    for invalid in [
        "",
        "a b",
        "a\tb",
        "a\nb",
        "a\u{007f}b",
        "é",
        &"x".repeat(MAX_QDRANT_API_KEY_BYTES + 1),
    ] {
        assert!(matches!(
            vault.save(input.clone(), Secret::new(invalid.into())),
            Err(Error::InvalidInput(_))
        ));
    }
    for scoped in [
        CredentialInput {
            username: "fixture".into(),
            ..input.clone()
        },
        CredentialInput {
            server_pattern: "fixture.test".into(),
            ..input.clone()
        },
        CredentialInput {
            environment: "fixture".into(),
            ..input.clone()
        },
    ] {
        assert!(matches!(
            vault.save(scoped, Secret::new("fixture-key".into())),
            Err(Error::InvalidInput(_))
        ));
    }
    let key = "x".repeat(MAX_QDRANT_API_KEY_BYTES);
    let id = vault.save(input.clone(), Secret::new(key)).unwrap();
    assert!(valid_credential_id(&id));
    assert!(vault.qdrant_api_key(&ssh).is_none());
    assert!(vault.huggingface_token(&id).is_none());
    assert!(matches!(
        vault.save(
            CredentialInput {
                id: Some(id.clone()),
                kind: CredentialKind::Password,
                ..input.clone()
            },
            Secret::new("wrong-purpose".into())
        ),
        Err(Error::InvalidInput(_))
    ));
    let rotated = "fixture-key-JWT.012_=-!";
    assert_eq!(
        vault
            .save(
                CredentialInput {
                    id: Some(id.clone()),
                    ..input
                },
                Secret::new(rotated.into())
            )
            .unwrap(),
        id
    );
    vault.lock();
    let reopened = Vault::unlock(&path, MASTER).unwrap();
    assert_eq!(reopened.qdrant_api_key(&id), Some(rotated));
    assert_eq!(
        reopened.get(&id).unwrap().kind,
        CredentialKind::QdrantApiKey
    );
    assert!(!reopened.get(&id).unwrap().kind.is_ssh());
    assert_eq!(reopened.secret(&ssh), Some(TEST_SECRET));
    assert!(reopened.qdrant_api_key(&ssh).is_none());
}

#[test]
fn huggingface_token_retains_its_dedicated_purpose_after_unlock() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let input = CredentialInput {
        label: "Test Hub token".into(),
        kind: CredentialKind::HuggingFaceToken,
        ..Default::default()
    };
    let token = "hf_test_token";
    assert!(matches!(
        vault.save(input.clone(), Secret::new("a\nb".into())),
        Err(Error::InvalidInput(_))
    ));
    let id = vault
        .save(input.clone(), Secret::new(token.into()))
        .unwrap();
    assert!(vault.archive_password(&id).is_none());
    assert!(matches!(
        vault.save(
            CredentialInput {
                id: Some(id.clone()),
                kind: CredentialKind::Password,
                ..input
            },
            Secret::new(token.into()),
        ),
        Err(Error::InvalidInput(_))
    ));
    vault.lock();
    let vault = Vault::unlock(&path, MASTER).unwrap();
    assert_eq!(vault.huggingface_token(&id), Some(token));
    assert!(!vault.get(&id).unwrap().kind.is_ssh());
}

#[test]
fn archive_password_keeps_its_purpose_and_id_when_the_master_password_changes() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let input = CredentialInput {
        label: "Test archive".into(),
        kind: CredentialKind::ArchivePassword,
        ..Default::default()
    };
    for password in ["  ".to_owned(), "x".repeat(1025)] {
        assert!(matches!(
            vault.save(input.clone(), Secret::new(password)),
            Err(Error::InvalidInput(_))
        ));
    }
    let id = vault
        .save(input.clone(), Secret::new(TEST_SECRET.into()))
        .unwrap();
    let ssh = vault
        .save(
            CredentialInput {
                label: "Test SSH".into(),
                ..Default::default()
            },
            Secret::new("other test secret".into()),
        )
        .unwrap();
    assert!(vault.archive_password(&ssh).is_none());
    assert!(matches!(
        vault.save(
            CredentialInput {
                id: Some(id.clone()),
                kind: CredentialKind::Password,
                ..input
            },
            Secret::new(TEST_SECRET.into())
        ),
        Err(Error::InvalidInput(_))
    ));
    assert!(
        !fs::read(&path)
            .unwrap()
            .windows(TEST_SECRET.len())
            .any(|bytes| bytes == TEST_SECRET.as_bytes())
    );
    vault.change_master_password(MASTER, CHANGED).unwrap();
    vault.lock();
    let vault = Vault::unlock(path, CHANGED).unwrap();
    assert_eq!(vault.archive_password(&id), Some(TEST_SECRET));
    assert!(vault.archive_password(&ssh).is_none());
}

#[test]
fn saves_edits_rotates_and_restores_a_backup() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let backup = directory.0.join("backup");
    assert!(!Vault::exists(&path).unwrap());
    assert!(matches!(Vault::unlock(&path, MASTER), Err(Error::NotFound)));
    assert!(matches!(
        Vault::create(&path, b"short"),
        Err(Error::InvalidInput(_))
    ));
    let mut vault = Vault::create(&path, MASTER).unwrap();
    assert!(Vault::exists(&path).unwrap());
    assert!(matches!(Vault::unlock(&path, MASTER), Err(Error::Busy)));
    let empty = fs::read(&path).unwrap();
    let input = CredentialInput {
        label: "test ssh credential".into(),
        username: "test-user".into(),
        server_pattern: "*.Example.test.".into(),
        environment: "test environment".into(),
        description: "test-only description".into(),
        ..Default::default()
    };
    let id = vault
        .save(input.clone(), Secret::new(TEST_SECRET.into()))
        .unwrap();
    let key_id = vault
        .save(
            CredentialInput {
                label: "test passphrase".into(),
                kind: CredentialKind::KeyPassphrase,
                ..Default::default()
            },
            Secret::new("test passphrase secret".into()),
        )
        .unwrap();
    assert_eq!(vault.entries().len(), 2);
    let credential = vault.get(&id).unwrap();
    assert!(credential.matches_host("HOST.example.TEST."));
    assert!(credential.matches_host("nested.host.example.test"));
    assert!(!credential.matches_host("example.test"));
    assert!(!credential.matches_host("host.example.test.invalid"));
    assert!(!credential.matches_host("host.example.test/path"));
    let original = fs::read(&path).unwrap();
    assert_ne!(empty, original);
    for confidential in [
        TEST_SECRET,
        &input.label,
        &input.username,
        &input.server_pattern,
        &input.environment,
        &input.description,
    ] {
        assert!(
            !original
                .windows(confidential.len())
                .any(|slice| slice == confidential.as_bytes())
        );
    }
    vault.backup_to(&backup).unwrap();
    assert!(matches!(
        vault.backup_to(&backup),
        Err(Error::AlreadyExists)
    ));
    assert_eq!(fs::read(&backup).unwrap(), original);

    let edited = CredentialInput {
        id: Some(id.clone()),
        description: "updated description".into(),
        ..input
    };
    assert_eq!(
        vault
            .save(edited, Secret::new("edited test secret".into()))
            .unwrap(),
        id
    );
    assert!(vault.delete(&key_id).unwrap());
    assert!(!vault.delete(&key_id).unwrap());
    assert!(matches!(
        vault.change_master_password(WRONG, CHANGED),
        Err(Error::Authentication)
    ));
    vault.change_master_password(MASTER, CHANGED).unwrap();
    vault.lock();
    assert!(matches!(
        Vault::create(&path, MASTER),
        Err(Error::AlreadyExists)
    ));
    assert!(matches!(
        Vault::unlock(&path, MASTER),
        Err(Error::Authentication)
    ));
    let vault = Vault::unlock(&path, CHANGED).unwrap();
    assert_eq!(vault.entries().len(), 1);
    assert_eq!(vault.secret(&id), Some("edited test secret"));
    assert_eq!(vault.get(&id).unwrap().description, "updated description");
    let destination = directory.0.join("restored");
    let mut restored = Vault::restore_from(&destination, &backup, MASTER).unwrap();
    assert_eq!(restored.secret(&id), Some(TEST_SECRET));
    assert_eq!(
        restored.get(&key_id).unwrap().kind,
        CredentialKind::KeyPassphrase
    );
    assert_eq!(fs::read(&destination).unwrap(), original);
    assert!(restored.delete(&key_id).unwrap());
    assert_eq!(fs::read(&backup).unwrap(), original);
    restored.lock();
    let restored = Vault::unlock(&destination, MASTER).unwrap();
    assert_eq!(restored.secret(&id), Some(TEST_SECRET));
    assert!(restored.get(&key_id).is_none());
}

#[test]
fn restore_does_not_publish_invalid_backups_or_replace_a_vault() {
    let directory = Directory::new();
    let source = directory.0.join("source");
    let backup = directory.0.join("backup");
    let destination = directory.0.join("restored");
    let vault = Vault::create(&source, MASTER).unwrap();
    vault.backup_to(&backup).unwrap();
    vault.lock();
    let original = fs::read(&source).unwrap();

    assert!(matches!(
        Vault::restore_from(&source, &backup, MASTER),
        Err(Error::AlreadyExists)
    ));
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(matches!(
        Vault::restore_from(&destination, &backup, WRONG),
        Err(Error::Authentication)
    ));
    assert!(!Vault::exists(&destination).unwrap());

    let mut damaged = fs::read(&backup).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    fs::write(&backup, damaged).unwrap();
    assert!(matches!(
        Vault::restore_from(&destination, &backup, MASTER),
        Err(Error::Authentication)
    ));
    assert!(!Vault::exists(&destination).unwrap());
    assert!(!directory.0.join("restored.lock").exists());
}

#[test]
fn tampering_cannot_be_unlocked_or_overwritten() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let mut vault = Vault::create(&path, MASTER).unwrap();
    let input = CredentialInput {
        label: "test".into(),
        ..Default::default()
    };
    let id = vault
        .save(input.clone(), Secret::new(TEST_SECRET.into()))
        .unwrap();
    let original = fs::read(&path).unwrap();
    let mut damaged = original.clone();
    *damaged.last_mut().unwrap() ^= 1;
    fs::write(&path, &damaged).unwrap();
    assert!(matches!(
        vault.save(input, Secret::new("another test secret".into())),
        Err(Error::Changed)
    ));
    assert!(matches!(vault.delete(&id), Err(Error::Changed)));
    assert_eq!(vault.entries().len(), 1);
    vault.lock();
    assert_eq!(fs::read(&path).unwrap(), damaged);
    assert!(matches!(
        Vault::unlock(&path, MASTER),
        Err(Error::Authentication)
    ));

    // Corrupt the encoded KDF memory cost and reject it before key derivation.
    let mut invalid = original.clone();
    invalid[9] = 0xff;
    fs::write(&path, invalid).unwrap();
    assert!(matches!(
        Vault::unlock(&path, MASTER),
        Err(Error::InvalidFormat)
    ));
    fs::write(&path, &original[..8]).unwrap();
    assert!(matches!(
        Vault::unlock(&path, MASTER),
        Err(Error::InvalidFormat)
    ));
    fs::File::create(&path)
        .unwrap()
        .set_len(4 * 1024 * 1024 + 1)
        .unwrap();
    assert!(matches!(Vault::unlock(&path, MASTER), Err(Error::Limit)));
}

#[test]
fn competing_creates_never_clobber_a_vault() {
    let directory = Directory::new();
    let path = directory.0.join("vault");
    let start = Arc::new(Barrier::new(2));
    let creators = [MASTER, CHANGED].map(|password| {
        let path = path.clone();
        let start = Arc::clone(&start);
        thread::spawn(move || {
            start.wait();
            Vault::create(path, password)
        })
    });
    let outcomes = creators.map(|worker| worker.join().unwrap());
    let mut winners = 0;
    for (outcome, password) in outcomes.into_iter().zip([MASTER, CHANGED]) {
        match outcome {
            Ok(vault) => {
                winners += 1;
                vault.lock();
                assert!(Vault::unlock(&path, password).is_ok());
            }
            Err(Error::Busy | Error::AlreadyExists) => {}
            Err(error) => panic!("unexpected create error {error}"),
        }
    }
    assert_eq!(winners, 1);
}

#[cfg(unix)]
#[test]
fn refuses_vault_symlinks_and_uses_private_permissions() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let directory = Directory::new();
    let path = directory.0.join("vault");
    Vault::create(&path, MASTER).unwrap().lock();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let symlink_path = directory.0.join("symlink");
    symlink(&path, &symlink_path).unwrap();
    assert!(matches!(
        Vault::exists(&symlink_path),
        Err(Error::InvalidFormat)
    ));
    assert!(matches!(
        Vault::unlock(&symlink_path, MASTER),
        Err(Error::InvalidFormat)
    ));
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir();
        for _ in 0..256 {
            let next = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!("kortty-vault-test-{}-{next}", std::process::id()));
            match create_test_directory(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("could not create vault test directory: {error}"),
            }
        }
        panic!("could not create a unique vault test directory");
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn create_test_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(windows)]
    {
        fs::create_dir(path)
    }
}
