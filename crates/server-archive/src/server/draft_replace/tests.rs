use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;

use kortty_server_archive::draft_replace::Original;

use super::*;

const ORIGINAL_TEXT: &[u8] = b"\xef\xbb\xbfreviewed original\r\n";
const DRAFT_TEXT: &[u8] = b"generated replacement\n";

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fixture(backup: bool) -> (Scratch, Spec) {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    let scratch = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("kortty-draft-fixture-{}", hex::encode(nonce)));
    fs::DirBuilder::new().mode(0o700).create(&scratch).unwrap();
    let scratch = Scratch(scratch);
    let path = scratch.0.join("settings.txt");
    fs::write(&path, ORIGINAL_TEXT).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let spec = Spec {
        destination: path.to_str().unwrap().into(),
        original: Original {
            bytes: ORIGINAL_TEXT.len() as u64,
            digest: digest(ORIGINAL_TEXT),
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: 0o750,
        },
        draft_bytes: DRAFT_TEXT.len(),
        draft_digest: digest(DRAFT_TEXT),
        backup,
    };
    (scratch, spec)
}

#[test]
fn reviewed_draft_replacement_preserves_raw_backup_mode_and_user_attributes() {
    for backup in [false, true] {
        let (scratch, spec) = fixture(backup);
        let original = File::open(&spec.destination).unwrap();
        fsetxattr(
            &original,
            "user.kortty-fixture",
            b"metadata",
            XattrFlags::empty(),
        )
        .unwrap();
        let reviewed = review(spec.clone()).unwrap();
        reviewed.validate_for(&spec).unwrap();
        let result = apply(&spec, &reviewed.token, DRAFT_TEXT).unwrap();
        assert_eq!(
            result,
            Reply::Applied {
                destination: spec.destination.clone(),
                draft_digest: spec.draft_digest,
                backup: spec.backup_path().unwrap(),
                recovery: None,
            }
        );
        for (path, expected) in [
            (Some(spec.destination.clone()), DRAFT_TEXT),
            (reviewed.backup, ORIGINAL_TEXT),
        ] {
            if let Some(path) = path {
                let file = File::open(&path).unwrap();
                assert_eq!(fs::read(path).unwrap(), expected);
                assert_eq!(file.metadata().unwrap().mode() & 0o7777, 0o750);
                assert_eq!(read_xattrs(&file).unwrap(), read_xattrs(&original).unwrap());
            }
        }
        assert_eq!(
            fs::read_dir(&scratch.0).unwrap().count(),
            usize::from(backup) + 1
        );
        assert!(apply(&spec, &reviewed.token, DRAFT_TEXT).is_err());
    }
}

#[test]
fn draft_review_rejects_same_bytes_in_a_different_inode_hard_links_and_backup_collision() {
    let (scratch, spec) = fixture(true);
    let reviewed = review(spec.clone()).unwrap();
    let original = scratch.0.join("previous");
    fs::rename(&spec.destination, &original).unwrap();
    fs::copy(&original, &spec.destination).unwrap();
    assert!(apply(&spec, &reviewed.token, DRAFT_TEXT).is_err());
    let hard_link = scratch.0.join("hard-link");
    fs::hard_link(&spec.destination, &hard_link).unwrap();
    assert!(review(spec.clone()).is_err());
    fs::remove_file(hard_link).unwrap();
    let backup = spec.backup_path().unwrap().unwrap();
    fs::write(&backup, b"existing backup").unwrap();
    assert!(review(spec.clone()).is_err());
    assert_eq!(fs::read(backup).unwrap(), b"existing backup");
    assert_eq!(fs::read(&spec.destination).unwrap(), ORIGINAL_TEXT);
}

#[test]
fn draft_quarantine_restores_substitution_without_publishing() {
    let (scratch, spec) = fixture(true);
    let reviewed = review(spec.clone()).unwrap();
    let prior = scratch.0.join("prior");
    let result = apply_with(&spec, &reviewed.token, DRAFT_TEXT, |phase| {
        if phase == Phase::Quarantine {
            fs::rename(&spec.destination, &prior)?;
            fs::write(&spec.destination, b"replacement by another process")?;
        }
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        result,
        Reply::Failed {
            outcome_unknown: false,
            backup: None,
            recovery: None,
            ..
        }
    ));
    assert_eq!(
        fs::read(&spec.destination).unwrap(),
        b"replacement by another process"
    );
    assert_eq!(fs::read(prior).unwrap(), ORIGINAL_TEXT);
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 2);
}

#[test]
fn draft_late_backup_collision_restores_original_and_retains_existing_backup() {
    let (scratch, spec) = fixture(true);
    let reviewed = review(spec.clone()).unwrap();
    let backup = reviewed.backup.as_ref().unwrap();
    let result = apply_with(&spec, &reviewed.token, DRAFT_TEXT, |phase| {
        if phase == Phase::Backup {
            fs::write(backup, b"backup created by another process")?;
        }
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        result,
        Reply::Failed {
            outcome_unknown: false,
            backup: None,
            recovery: None,
            ..
        }
    ));
    assert_eq!(
        fs::read(backup).unwrap(),
        b"backup created by another process"
    );
    assert_eq!(fs::read(&spec.destination).unwrap(), ORIGINAL_TEXT);
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 2);
}

#[test]
fn draft_late_destination_collision_retains_occupant_backup_and_recovery() {
    let (_scratch, spec) = fixture(true);
    let reviewed = review(spec.clone()).unwrap();
    let recovery = spec.recovery_path(&reviewed.token).unwrap();
    let result = apply_with(&spec, &reviewed.token, DRAFT_TEXT, |phase| {
        if phase == Phase::Publish {
            fs::write(&spec.destination, b"late occupant")?;
        }
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        result,
        Reply::Failed { outcome_unknown: true, backup: Some(ref backup), recovery: Some(ref path), .. }
            if Some(backup) == reviewed.backup.as_ref() && path == &recovery
    ));
    assert_eq!(fs::read(&spec.destination).unwrap(), b"late occupant");
    assert_eq!(fs::read(reviewed.backup.unwrap()).unwrap(), ORIGINAL_TEXT);
    assert_eq!(fs::read(recovery).unwrap(), ORIGINAL_TEXT);
}
