use super::*;

fn open(kind: Kind) -> (Workspace, window::Id) {
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let message = match kind {
        Kind::Export => Message::OpenExport,
        Kind::Import => Message::OpenImport,
    };
    let _ = workspace.update_backup(owner, message);
    let dialog = workspace.backup_for_owner(owner).unwrap();
    (workspace, dialog)
}

fn fixture_path() -> String {
    std::env::temp_dir()
        .join("not-written-by-the-backup-ui-test.zip")
        .into_os_string()
        .into_string()
        .unwrap()
}

#[test]
fn backup_ui_create_checks_absolute_path_and_matches_utf8_password_bytes() {
    let (mut workspace, id) = open(Kind::Export);
    assert_eq!(workspace.backup_title(id), Some("Create Backup"));
    assert!(workspace.backup_view(id).is_some());
    let _ = workspace.update_backup(id, Message::Path("relative.zip".into()));
    let value = "é".repeat(MIN_PASSWORD_BYTES / 2);
    let _ = workspace.update_backup(id, Message::Password(Password::new(value.clone())));
    let _ = workspace.update_backup(id, Message::Repeat(Password::new(value.clone())));
    assert!(!workspace.backup.0.as_ref().unwrap().can_submit());

    let _ = workspace.update_backup(id, Message::Path(fixture_path()));
    let _ = workspace.update_backup(id, Message::Repeat(Password::new("different".into())));
    assert!(!workspace.backup.0.as_ref().unwrap().can_submit());
    let _ = workspace.update_backup(id, Message::Repeat(Password::new(value)));
    assert!(workspace.backup.0.as_ref().unwrap().can_submit());
    let _ = workspace.update_backup(id, Message::Submit);
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(dialog.phase, Phase::Working(Work::Export)));
    assert!(dialog.password.is_empty() && dialog.repeat.is_empty());
    let _ = workspace.update_backup(id, Message::Cancel);
    assert!(workspace.backup.is_open());
}

#[test]
fn backup_ui_oversized_replacements_clear_the_previous_path_and_passwords() {
    let (mut workspace, id) = open(Kind::Export);
    let password = || Password::new("fixture-only password".into());
    let oversized = || Password::new("é".repeat(MAX_ARCHIVE_PASSWORD_BYTES / 2 + 1));
    let _ = workspace.update_backup(id, Message::Path(fixture_path()));
    let _ = workspace.update_backup(id, Message::Password(password()));
    let _ = workspace.update_backup(id, Message::Repeat(password()));

    let _ = workspace.update_backup(id, Message::Path("x".repeat(MAX_PATH_BYTES + 1)));
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(dialog.path.is_empty() && dialog.error.is_some() && !dialog.can_submit());
    let _ = workspace.update_backup(id, Message::Path(fixture_path()));
    assert!(workspace.backup.0.as_ref().unwrap().can_submit());

    let _ = workspace.update_backup(id, Message::Password(oversized()));
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(dialog.password.is_empty() && dialog.error.is_some() && !dialog.can_submit());
    let _ = workspace.update_backup(id, Message::Password(password()));
    assert!(workspace.backup.0.as_ref().unwrap().can_submit());

    let _ = workspace.update_backup(id, Message::Repeat(oversized()));
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(dialog.repeat.is_empty() && dialog.error.is_some() && !dialog.can_submit());
    let _ = workspace.update_backup(id, Message::Submit);
    assert!(matches!(
        workspace.backup.0.as_ref().unwrap().phase,
        Phase::Form
    ));
}

#[test]
fn backup_ui_dirty_import_close_and_failed_stale_reply_do_not_queue() {
    let (mut workspace, id) = open(Kind::Import);
    assert_eq!(workspace.backup_title(id), Some("Import Backup"));
    let _ = workspace.update_backup(id, Message::Restore(0));
    assert!(matches!(
        workspace.backup.0.as_ref().unwrap().phase,
        Phase::Form
    ));
    let _ = workspace.update_backup(id, Message::Path(fixture_path()));
    let _ = workspace.update_backup(id, Message::ImportFormat(ImportFormat::Password));
    let _ = workspace.update_backup(
        id,
        Message::Password(Password::new("fixture-only password".into())),
    );
    let _ = workspace.close_backup(id);
    assert!(workspace.backup.0.as_ref().unwrap().discard == Some(Discard::Close));
    let _ = workspace.update_backup(id, Message::KeepEditing);
    assert!(workspace.backup.0.as_ref().unwrap().can_submit());

    let _ = workspace.update_backup(id, Message::Submit);
    let revision = workspace.backup.0.as_ref().unwrap().revision;
    // Test-double worker reply. The returned Iced task is intentionally not executed.
    let reply = Reply(Arc::new(Mutex::new(Some(Err(
        "test-only authentication failure".into(),
    )))));
    let _ = workspace.update_backup(
        id,
        Message::Finished(revision.wrapping_sub(1), reply.clone()),
    );
    assert!(workspace.backup.0.as_ref().unwrap().busy());
    let _ = workspace.update_backup(id, Message::Finished(revision, reply.clone()));
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(dialog.phase, Phase::Form));
    assert!(
        dialog
            .error
            .as_ref()
            .unwrap()
            .contains("test-only authentication failure")
    );
    assert!(reply.0.lock().unwrap().is_none());
    assert!(dialog.password.is_empty());

    let _ = workspace.close_backup(id);
    let _ = workspace.update_backup(id, Message::Discard);
    assert!(!workspace.backup.is_open());
}

#[test]
fn backup_ui_openpgp_import_detects_before_asking_for_an_absolute_key() {
    let (mut workspace, id) = open(Kind::Import);
    let archive = std::env::temp_dir().join("renamed-fixture.zip");
    let key = std::env::temp_dir().join("fixture-secret-key.asc");
    let _ = workspace.update_backup(id, Message::Path(archive.to_str().unwrap().into()));
    let _ = workspace.update_backup(id, Message::Submit);
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(dialog.phase, Phase::Working(Work::Detect)));
    let revision = dialog.revision;
    // Test-double worker result. The codec tests identify real renamed archives.
    let reply = Reply(Arc::new(Mutex::new(Some(Ok(Outcome::Detected(
        BackupFormat::OpenPgp,
    ))))));
    let _ = workspace.update_backup(id, Message::Finished(revision, reply));
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert_eq!(dialog.encryption(), EncryptionMode::OpenPgp);
    assert!(!dialog.can_submit());
    assert!(workspace.backup_view(id).is_some());

    let _ = workspace.update_backup(id, Message::BrowsePrivateKey);
    assert_eq!(
        workspace.backup_title(id),
        Some("Select OpenPGP Private Key")
    );
    let _ = workspace.update_backup(id, Message::Back);
    let _ = workspace.update_backup(id, Message::PrivateKeyPath("relative.asc".into()));
    assert!(!workspace.backup.0.as_ref().unwrap().can_submit());
    let _ = workspace.update_backup(id, Message::PrivateKeyPath(key.to_str().unwrap().into()));
    assert!(workspace.backup.0.as_ref().unwrap().can_submit());
    let oversized = Password::new("x".repeat(MAX_ARCHIVE_PASSWORD_BYTES + 1));
    let _ = workspace.update_backup(id, Message::Password(oversized));
    assert!(!workspace.backup.0.as_ref().unwrap().can_submit());
    let _ = workspace.update_backup(id, Message::Password(Password::new("fixture key".into())));

    let _ = workspace.update_backup(id, Message::Path(fixture_path()));
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(dialog.needs_detection());
    assert!(dialog.password.is_empty() && dialog.private_key.is_empty());
    let _ = workspace.update_backup(id, Message::ImportFormat(ImportFormat::OpenPgp));
    let _ = workspace.update_backup(id, Message::PrivateKeyPath(key.to_str().unwrap().into()));
    let _ = workspace.update_backup(id, Message::Submit);
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(dialog.phase, Phase::Working(Work::Inspect)));
    assert!(dialog.password.is_empty());
}

#[test]
fn backup_ui_openpgp_export_defaults_to_gpg_and_blocks_missing_recipient() {
    let mut workspace = Workspace {
        backup_preferences: Preferences {
            encryption: EncryptionMode::OpenPgp,
            credential: Some("a".repeat(32)),
            recipient: Some("B".repeat(40)),
            ..Preferences::default()
        },
        backup_progress: Progress {
            last_directory: Some(std::env::temp_dir()),
            ..Progress::default()
        },
        ..Workspace::default()
    };
    let (owner, _) = workspace.open_window(false);
    let _ = workspace.update_backup(owner, Message::OpenExport);
    let id = workspace.backup_for_owner(owner).unwrap();
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert_eq!(dialog.encryption(), EncryptionMode::OpenPgp);
    assert_eq!(
        dialog.input_path().unwrap().file_name().unwrap(),
        "kortty-backup.zip.gpg",
    );
    assert!(dialog.selected_password().is_none());
    let _ = workspace.update_backup(id, Message::Submit);
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(dialog.phase, Phase::Form));
    assert!(dialog.error.as_deref().unwrap().contains("unavailable"));
}

fn java_backup_preview(root: &Path, entries: &[(&str, &[u8])]) -> Preview {
    use std::io::Write;
    use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

    let archive = root.join("java-backup.zip");
    let mut zip = ZipWriter::new(std::fs::File::create(&archive).unwrap());
    let options = SimpleFileOptions::default().with_aes_encryption_bytes(AesMode::Aes256, b"short");
    for &(path, bytes) in entries {
        zip.start_file(path, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    profile_backup::inspect_java_password(&archive, &Secret::new("short".into())).unwrap()
}

fn java_password_preview(root: &Path, encrypted: &str) -> Preview {
    let credentials = format!(
        "<credentials><credential><id>fixture-id</id><name>Fixture</name><username>fixture</username><encryptedPassword>{encrypted}</encryptedPassword><passwordType>STORED</passwordType></credential></credentials>"
    );
    java_backup_preview(root, &[
        ("connections.xml", br#"<connections><connection id="route"><name>Fixture</name><host>fixture.example</host><username>fixture</username></connection></connections>"#),
        ("credentials.xml", credentials.as_bytes()),
    ])
}

#[test]
fn backup_ui_java_password_review_preserves_preview_and_vault_on_failure() {
    let fixture = crate::snippets::tests::Profile::new();
    let preview = java_password_preview(&fixture.0, "ciphertext-placeholder");
    assert!(preview.has_java_passwords() && preview.count() > 0);
    assert!(preview.java_password_summary().is_none());

    let (mut workspace, id) = open(Kind::Import);
    let dialog = workspace.backup.0.as_mut().unwrap();
    dialog.phase = Phase::Review(preview);
    dialog.restore_mode = RestoreMode::JavaAdditive;
    let revision = dialog.revision;
    let _ = workspace.update_backup(
        id,
        Message::JavaMasterPassword(revision.wrapping_add(1), Password::new("stale".into())),
    );
    assert!(
        !workspace
            .backup
            .0
            .as_ref()
            .unwrap()
            .can_review_java_credentials(revision, JavaCredentialReview::WithVault)
    );
    let _ = workspace.update_backup(
        id,
        Message::JavaMasterPassword(revision, Password::new("short".into())),
    );
    assert!(
        workspace
            .backup
            .0
            .as_ref()
            .unwrap()
            .can_review_java_credentials(revision, JavaCredentialReview::WithVault)
    );
    let _ = workspace.update_backup(
        id,
        Message::ReviewJavaCredentials(revision, JavaCredentialReview::WithVault),
    );
    assert!(matches!(
        workspace.backup.0.as_ref().unwrap().phase,
        Phase::Review(_)
    ));
    assert!(
        workspace
            .backup
            .0
            .as_ref()
            .unwrap()
            .java_master_password
            .is_empty()
    );
    assert!(workspace.backup_view(id).is_some());

    let path = fixture.0.join("vault.bin");
    let vault = Vault::create(&path, b"fixture native password").unwrap();
    workspace.vault = Some(VaultWindow::with_unlocked_fixture(path, vault));
    let _ = workspace.update_backup(
        id,
        Message::JavaMasterPassword(revision, Password::new("short".into())),
    );
    let vault = workspace
        .vault
        .as_mut()
        .unwrap()
        .take_for_ssh_keys()
        .unwrap();
    let dialog = workspace.backup.0.as_mut().unwrap();
    let job = dialog.begin_java_credential_review(Some(vault), fixture.0.clone());
    assert!(dialog.java_master_password.is_empty());
    let revision = dialog.revision;
    assert!(workspace.vault.as_ref().unwrap().busy());
    let reply = JavaCredentialsReply(Arc::new(Mutex::new(Some(JavaCredentialsReviewed {
        vault: job.vault,
        review: Some((job.preview, Err("fixture retry"))),
    }))));
    let _ = workspace.update_backup(id, Message::JavaCredentialsReviewed(revision, reply));
    assert!(workspace.vault.as_ref().unwrap().can_manage_environments());
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(&dialog.phase, Phase::Review(preview) if preview.count() > 0));
    assert_eq!(dialog.error.as_deref(), Some("fixture retry"));
    assert!(dialog.java_master_password.is_empty());
    let _ = workspace.update_backup(id, Message::Back);
    let dialog = workspace.backup.0.as_ref().unwrap();
    assert!(matches!(dialog.phase, Phase::Form));
    assert!(dialog.java_master_password.is_empty());
}

#[test]
fn backup_ui_java_password_worker_returns_a_second_preview_and_unlock_state() {
    let fixture = crate::snippets::tests::Profile::new();
    let envelope = include_str!("../../profile_backup/java_secrets/fixtures/envelopes.txt")
        .lines()
        .next()
        .unwrap();
    let preview = java_password_preview(&fixture.0, envelope);
    let (mut workspace, id) = open(Kind::Import);
    let path = crate::profile::vault_path_at(&fixture.0);
    crate::private_files::Directory::open(path.parent().unwrap(), true).unwrap();
    let vault = Vault::create(&path, b"fixture native password").unwrap();
    workspace.vault = Some(VaultWindow::with_unlocked_fixture(path, vault));
    let vault = workspace
        .vault
        .as_mut()
        .unwrap()
        .take_for_ssh_keys()
        .unwrap();
    let dialog = workspace.backup.0.as_mut().unwrap();
    dialog.phase = Phase::Review(preview);
    dialog.restore_mode = RestoreMode::JavaAdditive;
    dialog.edit_java_master_password(dialog.revision, Password::new("päss🔑例".into()));
    let job = dialog.begin_java_credential_review(Some(vault), fixture.0.clone());
    let revision = dialog.revision;
    let outcome = job.prepare();
    let reply = JavaCredentialsReply(Arc::new(Mutex::new(Some(outcome))));
    let _ = workspace.update_backup(id, Message::JavaCredentialsReviewed(revision, reply));
    assert!(workspace.vault.as_ref().unwrap().can_manage_environments());
    let dialog = workspace.backup.0.as_ref().unwrap();
    let Phase::Review(preview) = &dialog.phase else {
        panic!("expected second Java preview")
    };
    assert_eq!(
        preview.java_password_summary(),
        Some((1, 0)),
        "{:?}",
        dialog.error
    );
    assert!(!dialog.offers_java_credential_review(JavaCredentialReview::WithVault));
    assert!(dialog.java_master_password.is_empty());
    assert!(workspace.backup_view(id).is_some());
}

#[test]
fn backup_ui_java_managed_key_worker_reviews_unencrypted_keys_without_a_vault() {
    let fixture = crate::snippets::tests::Profile::new();
    let preview = java_backup_preview(
        &fixture.0,
        &[
            (
                "ssh-keys.xml",
                include_bytes!(
                    "../../profile_backup/java_secrets/fixtures/java-managed-ssh-keys.xml"
                ),
            ),
            (
                "ssh-keys/f99ec24c-9aec-4978-a57f-ec37c449ca35_fixture-ed25519.pem",
                include_bytes!(
                    "../../profile_backup/java_secrets/fixtures/java-managed-ed25519.pem"
                ),
            ),
            (
                "ssh-keys/fe33d0a9-ab84-4351-8d5c-ece77f40432f_fixture-encrypted",
                include_bytes!(
                    "../../profile_backup/java_secrets/fixtures/java-managed-encrypted.key"
                ),
            ),
        ],
    );
    assert!(preview.has_java_managed_keys());
    assert_eq!(preview.count(), 0);
    let (mut workspace, id) = open(Kind::Import);
    let dialog = workspace.backup.0.as_mut().unwrap();
    dialog.phase = Phase::Review(preview);
    dialog.restore_mode = RestoreMode::JavaAdditive;
    let revision = dialog.revision;
    let mode = JavaCredentialReview::KeysWithoutPassphrases;
    assert!(dialog.can_review_java_credentials(revision, mode));
    assert!(!dialog.can_review_java_credentials(revision, JavaCredentialReview::WithVault));
    dialog.edit_java_master_password(revision, Password::new("unused".into()));
    let job = dialog.begin_java_credential_review(None, fixture.0.clone());
    assert!(job.source_password.is_none());
    assert!(dialog.java_master_password.is_empty());
    let revision = dialog.revision;
    let outcome = job.prepare();
    assert!(outcome.vault.is_none());
    let reply = JavaCredentialsReply(Arc::new(Mutex::new(Some(outcome))));
    let _ = workspace.update_backup(id, Message::JavaCredentialsReviewed(revision, reply));
    assert!(workspace.vault.is_none());
    let dialog = workspace.backup.0.as_ref().unwrap();
    let Phase::Review(preview) = &dialog.phase else {
        panic!("expected reviewed Java SSH keys")
    };
    assert_eq!(
        preview.java_key_summary(),
        Some((1, 1)),
        "{:?}",
        dialog.error
    );
    assert!(preview.count() > 0);
    assert!(!dialog.offers_java_credential_review(mode));
    assert!(!dialog.offers_java_credential_review(JavaCredentialReview::WithVault));
    let _ = workspace.update_backup(id, Message::Back);
    assert!(matches!(
        workspace.backup.0.as_ref().unwrap().phase,
        Phase::Form
    ));
}
