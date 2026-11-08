use std::fs;
use std::time::{Duration, UNIX_EPOCH};

use kortty_vault::Secret;
use zeroize::Zeroizing;

use super::*;
use crate::profile_backup::Snapshot;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let path = root.join(format!(
            "kortty-rotation-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn snapshot(value: &[u8]) -> Snapshot {
    Snapshot::from([(
        "window-settings.json".into(),
        Zeroizing::new(value.to_vec()),
    )])
}

fn at() -> Zoned {
    "2026-11-06T19:19:02Z"
        .parse::<jiff::Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::fixed(jiff::tz::offset(1)))
}

fn password() -> Secret {
    Secret::new("native test backup passphrase".into())
}

fn update(scratch: &Scratch, payload: &[u8], retention: u8) -> io::Result<Report> {
    write(&scratch.0, &snapshot(payload), &password(), retention, at())
}

fn pgp_update(
    scratch: &Scratch,
    recipient: &str,
    payload: &[u8],
    retention: u8,
) -> io::Result<Report> {
    write_with(
        &scratch.0,
        EncryptionMode::OpenPgp,
        retention,
        at(),
        |file| openpgp::write_to(file, &snapshot(payload), &scratch.0, recipient),
    )
}

#[test]
fn preserves_exact_previous_ciphertext_and_avoids_same_second_name_collisions() {
    let scratch = Scratch::new();
    let first = update(&scratch, b"first backup", 0).unwrap();
    assert_eq!(first.path, scratch.0.join(CURRENT_NAME));
    assert!(first.warning.is_none());
    let original = fs::read(&first.path).unwrap();
    let changed_password = Secret::new("changed backup passphrase".into());
    write(
        &scratch.0,
        &snapshot(b"second backup"),
        &changed_password,
        0,
        at(),
    )
    .unwrap();
    let second = fs::read(&first.path).unwrap();
    update(&scratch, b"third backup", 0).unwrap();
    let history = scratch.0.join(HISTORY);
    assert_eq!(
        fs::read(history.join("kortty-backup_2026-11-06_20-19-02.zip")).unwrap(),
        original
    );
    let rotated_second = history.join("kortty-backup_2026-11-06_20-19-02-1.zip");
    assert_eq!(fs::read(&rotated_second).unwrap(), second);
    assert_eq!(
        archive::read(&rotated_second, &changed_password).unwrap(),
        snapshot(b"second backup")
    );
    assert_eq!(
        archive::read(&first.path, &password()).unwrap(),
        snapshot(b"third backup")
    );
}

#[test]
fn prune_keeps_only_retained_verified_zip_and_never_counts_other_entries() {
    let scratch = Scratch::new();
    let first = update(&scratch, b"first backup", 0).unwrap();
    let original = fs::read(&first.path).unwrap();
    let history = scratch.0.join(HISTORY);
    fs::create_dir(&history).unwrap();
    let remove = history.join("kortty-backup_2099-11-05_12-00-00.zip");
    fs::write(&remove, &original).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&remove)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(600)))
        .unwrap();
    let gpg = history.join("kortty-backup_2026-11-05_12-00-00.zip.gpg");
    let invalid_name = history.join("kortty-backup_2026-02-30_12-00-00.zip");
    let foreign = history.join("kortty-backup_2026-11-05_13-00-00.zip");
    let current_gpg = scratch.0.join("kortty-backup.zip.gpg");
    fs::write(&gpg, b"old foreign gpg").unwrap();
    fs::write(&current_gpg, b"current foreign gpg").unwrap();
    fs::write(&invalid_name, &original).unwrap();
    fs::write(&foreign, b"foreign zip content").unwrap();

    assert!(update(&scratch, b"next", 1).unwrap().warning.is_none());
    assert!(!remove.exists());
    assert_eq!(fs::read(gpg).unwrap(), b"old foreign gpg");
    assert_eq!(fs::read(current_gpg).unwrap(), b"current foreign gpg");
    assert_eq!(fs::read(invalid_name).unwrap(), original);
    assert_eq!(fs::read(foreign).unwrap(), b"foreign zip content");
    assert_eq!(
        fs::read(history.join("kortty-backup_2026-11-06_20-19-02.zip")).unwrap(),
        original
    );
}

#[test]
fn unknown_current_backup_is_never_replaced_and_active_lock_serializes_writers() {
    let scratch = Scratch::new();
    let current = scratch.0.join(CURRENT_NAME);
    fs::write(&current, b"not a native backup").unwrap();
    assert!(update(&scratch, b"next", 0).is_err());
    assert_eq!(fs::read(&current).unwrap(), b"not a native backup");
    assert!(!scratch.0.join(HISTORY).exists());
    let directory = Directory::open(&scratch.0, false).unwrap();
    let lock = Lock::acquire(directory.open_lock(LOCK).unwrap()).unwrap();
    assert_eq!(
        update(&scratch, b"next", 0).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(lock);
    assert!(update(&scratch, b"next", 101).is_err());
    assert_eq!(fs::read(&current).unwrap(), b"not a native backup");
}

#[test]
fn a_missing_configured_destination_is_not_recreated() {
    let scratch = Scratch::new();
    let missing = scratch.0.join("removed-backup-folder");
    let error = write(&missing, &snapshot(b"fixture"), &password(), 0, at()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!missing.exists());
}

#[test]
fn failed_preservation_keeps_current_and_failed_cleanup_warns_after_publication() {
    let scratch = Scratch::new();
    let history = scratch.0.join(HISTORY);
    fs::write(&history, b"unrelated file blocking history").unwrap();
    let first = update(&scratch, b"first backup", 1).unwrap();
    assert!(first.warning.is_some());
    let original = fs::read(&first.path).unwrap();
    assert!(update(&scratch, b"second backup", 1).is_err());
    assert_eq!(fs::read(&first.path).unwrap(), original);
    assert_eq!(
        fs::read(history).unwrap(),
        b"unrelated file blocking history"
    );
    assert!(fs::read_dir(&scratch.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".stage-")
    }));
}

#[test]
fn failed_new_archive_never_preserves_or_replaces_the_previous_one() {
    let scratch = Scratch::new();
    let first = update(&scratch, b"first backup", 0).unwrap();
    let original = fs::read(&first.path).unwrap();
    let invalid = Snapshot::from([("../unsafe".into(), Zeroizing::new(Vec::new()))]);
    assert!(write(&scratch.0, &invalid, &password(), 0, at()).is_err());
    assert_eq!(fs::read(&first.path).unwrap(), original);
    assert!(!scratch.0.join(HISTORY).exists());
}

#[test]
fn native_mode_switch_preserves_both_current_backups_and_counts_both_formats() {
    let scratch = Scratch::new();
    let (recipient, _) = openpgp::tests::enroll(&scratch.0, pgp::types::KeyVersion::V4, 7);
    let zip = update(&scratch, b"password backup", 0).unwrap().path;
    let zip_bytes = fs::read(&zip).unwrap();

    let pgp = pgp_update(&scratch, &recipient, b"first OpenPGP backup", 0)
        .unwrap()
        .path;
    assert_eq!(pgp, scratch.0.join(OPENPGP_CURRENT_NAME));
    assert!(!zip.exists());
    let first_pgp = fs::read(&pgp).unwrap();
    let history = scratch.0.join(HISTORY);
    let first_zip = history.join("kortty-backup_2026-11-06_20-19-02.zip");
    assert_eq!(fs::read(&first_zip).unwrap(), zip_bytes);

    // Recreate a state with valid current backups in both formats.
    fs::write(&zip, &zip_bytes).unwrap();
    assert!(
        pgp_update(&scratch, &recipient, b"second OpenPGP backup", 0)
            .unwrap()
            .warning
            .is_none()
    );
    assert!(!zip.exists());
    let first_pgp_path = history.join("kortty-backup_2026-11-06_20-19-02.zip.gpg");
    assert_eq!(fs::read(&first_pgp_path).unwrap(), first_pgp);
    let second_zip_path = history.join("kortty-backup_2026-11-06_20-19-02-1.zip");
    assert_eq!(fs::read(&second_zip_path).unwrap(), zip_bytes);
    let second_pgp = fs::read(&pgp).unwrap();

    for path in [first_zip, first_pgp_path, second_zip_path] {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(600)))
            .unwrap();
    }
    assert!(
        update(&scratch, b"new password backup", 1)
            .unwrap()
            .warning
            .is_none()
    );
    assert!(!pgp.exists());
    assert_eq!(
        archive::read(&zip, &password()).unwrap(),
        snapshot(b"new password backup")
    );
    let remaining: Vec<_> = fs::read_dir(&history)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        remaining,
        [history.join("kortty-backup_2026-11-06_20-19-02-1.zip.gpg")]
    );
    assert_eq!(fs::read(&remaining[0]).unwrap(), second_pgp);
}

#[test]
fn failed_mode_change_keeps_current_and_refuses_foreign_requested_file() {
    let scratch = Scratch::new();
    let zip = update(&scratch, b"last successful backup", 1).unwrap().path;
    let original = fs::read(&zip).unwrap();
    let pgp = scratch.0.join(OPENPGP_CURRENT_NAME);
    assert!(
        write_with(&scratch.0, EncryptionMode::OpenPgp, 1, at(), |_| {
            // Fault-inject a failed encoder before any publication.
            Err(io::Error::other("test encoder failed"))
        })
        .is_err()
    );
    assert!(!pgp.exists());
    assert!(!scratch.0.join(HISTORY).exists());
    assert_eq!(fs::read(&zip).unwrap(), original);

    fs::write(&pgp, b"foreign legacy OpenPGP backup").unwrap();
    assert!(
        write_with(&scratch.0, EncryptionMode::OpenPgp, 1, at(), |_| {
            panic!("must reject occupied foreign requested slot before running the encoder")
        })
        .is_err()
    );
    assert_eq!(fs::read(&pgp).unwrap(), b"foreign legacy OpenPGP backup");
    assert_eq!(fs::read(zip).unwrap(), original);
    assert!(!scratch.0.join(HISTORY).exists());
}

#[test]
fn post_commit_cleanup_refuses_to_remove_a_changed_inactive_file() {
    let scratch = Scratch::new();
    let zip = update(&scratch, b"old password backup", 0).unwrap().path;
    let target = Directory::open(&scratch.0, false).unwrap();
    let inactive = Previous::open(&target, EncryptionMode::Password).unwrap();
    fs::write(&zip, b"another process replaced the inactive backup").unwrap();
    assert!(finish_rotation(&target, inactive, None, 1).is_err());
    assert_eq!(
        fs::read(zip).unwrap(),
        b"another process replaced the inactive backup"
    );
}

#[test]
fn a_requested_slot_replaced_during_encryption_is_left_intact() {
    let scratch = Scratch::new();
    let current = update(&scratch, b"native old backup", 0).unwrap().path;
    let old = fs::read(&current).unwrap();
    let moved = scratch.0.join("kept-by-another-process");
    let result = write_with(&scratch.0, EncryptionMode::Password, 0, at(), |file| {
        archive::write_to(file, &snapshot(b"new native backup"), &password())?;
        fs::rename(&current, &moved)?;
        fs::write(&current, b"foreign file arriving during encryption")
    });
    assert!(result.is_err());
    assert_eq!(
        fs::read(&current).unwrap(),
        b"foreign file arriving during encryption"
    );
    assert_eq!(fs::read(moved).unwrap(), old);
}

#[cfg(unix)]
#[test]
fn refuses_redirected_current_and_preserves_foreign_symlink_archives() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let foreign = Scratch::new();
    let original = update(&foreign, b"outside backup", 0).unwrap().path;
    let outside = fs::read(&original).unwrap();
    let current = scratch.0.join(CURRENT_NAME);
    symlink(&original, &current).unwrap();
    assert!(update(&scratch, b"redirected", 0).is_err());
    assert_eq!(fs::read(&original).unwrap(), outside);
    fs::remove_file(current).unwrap();

    let history = scratch.0.join(HISTORY);
    fs::create_dir(&history).unwrap();
    let link = history.join("kortty-backup_2026-11-06_20-19-02.zip");
    symlink(&original, &link).unwrap();
    update(&scratch, b"first", 1).unwrap();
    update(&scratch, b"second", 1).unwrap();
    assert!(link.is_symlink());
    assert!(
        history
            .join("kortty-backup_2026-11-06_20-19-02-1.zip")
            .is_file()
    );
    assert_eq!(fs::read(&original).unwrap(), outside);
}
