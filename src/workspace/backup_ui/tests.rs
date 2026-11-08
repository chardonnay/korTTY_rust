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
        EncryptionMode::OpenPgp,
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
