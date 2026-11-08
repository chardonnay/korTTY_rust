use std::cell::Cell;
use std::fs;

use super::*;
use crate::profile_backup::rotation::CURRENT_NAME;

const VAULT_ID: &str = "01010101010101010101010101010101";
const FIXTURE_SECRET: &str = "test-only profile archive password";
const STORED_SETTINGS: &[u8] = br#"{"version":1,"custom":[],"colors":{}}"#;

struct Fixture {
    root: PathBuf,
    folder: PathBuf,
    preferences: Preferences,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-backup-writer-{}",
            crate::private_files::nonce().unwrap()
        ));
        let directory = Directory::open(&root, true).unwrap();
        directory
            .publish_copy("environments.json", STORED_SETTINGS)
            .unwrap();
        directory.private_child("archive-output", true).unwrap();
        let folder = root.join("archive-output");
        let preferences = Preferences {
            retention: 2,
            credential: Some(VAULT_ID.into()),
            interval: Interval::Daily,
            directory: Some(folder.clone()),
            ..Preferences::default()
        };
        assert!(preferences::save(&root, &Preferences::default(), preferences.clone()).unwrap());
        Self {
            root,
            folder,
            preferences,
        }
    }

    fn run(
        &self,
        password: Result<Secret, &'static str>,
        time: &str,
        policy: impl Fn() -> ClipboardPolicy,
    ) -> io::Result<AutoOutcome> {
        run_automatic_for(
            &self.root,
            self.preferences.clone(),
            Some(password),
            None,
            policy,
            || Ok(at(time)),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn at(value: &str) -> Zoned {
    value.parse::<Timestamp>().unwrap().to_zoned(TimeZone::UTC)
}

fn secret() -> Secret {
    Secret::new(FIXTURE_SECRET.into())
}

#[test]
fn automatic_and_manual_backups_preserve_distinct_timestamps_and_rotate() {
    let fixture = Fixture::new();
    let run = "2026-03-07T15:22:00Z";
    let AutoOutcome::Written(first) = fixture
        .run(Ok(secret()), run, ClipboardPolicy::default)
        .unwrap()
    else {
        panic!("the first check should create the backup");
    };
    assert_eq!(first.progress.last_auto, Some(at(run).timestamp()));
    assert_eq!(first.progress.last_backup, first.progress.last_auto);
    assert_eq!(first.count, 1);
    let original = fs::read(&first.path).unwrap();
    assert_eq!(
        archive::read(&first.path, &secret()).unwrap()["environments.json"].as_slice(),
        STORED_SETTINGS
    );

    let manual = "2026-03-07T16:23:00Z";
    let second = export_for(
        &fixture.root,
        &first.path,
        &secret(),
        Some(VAULT_ID),
        || Ok(at(manual)),
    )
    .unwrap();
    assert_eq!(second.progress.last_backup, Some(at(manual).timestamp()));
    assert_eq!(second.progress.last_auto, first.progress.last_auto);
    let rotated = fixture
        .folder
        .join("old-backups/kortty-backup_2026-03-07_16-23-00.zip");
    assert_eq!(fs::read(rotated).unwrap(), original);
    assert_ne!(fs::read(&first.path).unwrap(), original);

    assert!(matches!(
        fixture
            .run(
                Ok(secret()),
                "2026-03-08T15:21:59Z",
                ClipboardPolicy::default
            )
            .unwrap(),
        AutoOutcome::Waiting(_)
    ));
    assert!(matches!(
        fixture
            .run(
                Ok(secret()),
                "2026-03-08T15:22:00Z",
                ClipboardPolicy::default
            )
            .unwrap(),
        AutoOutcome::Written(_)
    ));
    let (_, progress) = preferences::load(&fixture.root).unwrap();
    assert_eq!(
        progress.last_auto,
        Some(at("2026-03-08T15:22:00Z").timestamp())
    );
    assert_eq!(
        progress.last_directory.as_deref(),
        Some(fixture.folder.as_path())
    );
}

#[test]
fn scheduled_openpgp_uses_the_pinned_public_recipient_with_the_vault_locked() {
    use pgp::composed::SignedPublicKey;
    use pgp::types::{KeyDetails, KeyVersion};

    use crate::snippets::archive::test_openpgp_key;
    use crate::snippets::recipients::{Action, Change, FILE_NAME};

    let fixture = Fixture::new();
    let key = test_openpgp_key(KeyVersion::V4, 67);
    let cert = SignedPublicKey::from(key.clone());
    let fingerprint = format!("{:X}", cert.fingerprint());
    let public = fixture.root.join("fixture-public.asc");
    let private = fixture.root.join("fixture-private.asc");
    fs::write(&public, cert.to_armored_bytes(Default::default()).unwrap()).unwrap();
    Directory::open(&fixture.root, false)
        .unwrap()
        .publish_copy(
            "fixture-private.asc",
            &key.to_armored_bytes(Default::default()).unwrap(),
        )
        .unwrap();
    Action::Save(
        fixture.root.join(FILE_NAME),
        Change {
            previous: None,
            name: "Test-only backup recipient".into(),
            fingerprint: fingerprint.clone(),
            certificate: Some(public),
        },
    )
    .run()
    .unwrap();

    let next = Preferences {
        encryption: EncryptionMode::OpenPgp,
        recipient: Some(fingerprint),
        credential: None,
        ..fixture.preferences.clone()
    };
    preferences::save(&fixture.root, &fixture.preferences, next.clone()).unwrap();
    let now = at("2026-03-07T15:22:00Z");
    let AutoOutcome::Written(exported) = run_automatic_for(
        &fixture.root,
        next,
        None,
        None,
        ClipboardPolicy::default,
        || Ok(now),
    )
    .unwrap() else {
        panic!("the public recipient should suffice for a scheduled backup");
    };
    assert_eq!(
        exported.path,
        fixture
            .folder
            .join(rotation::current_name(EncryptionMode::OpenPgp))
    );
    let preview =
        crate::profile_backup::inspect_openpgp(&exported.path, &private, &Secret::default())
            .unwrap();
    assert_eq!(
        preview.entries().collect::<Vec<_>>(),
        ["environments.json", FILE_NAME]
    );
    assert!(exported.progress.last_auto.is_some());
}

#[test]
fn automatic_backup_never_falls_back_from_a_managed_folder_or_stale_policy() {
    let fixture = Fixture::new();
    let time = "2026-03-07T15:22:00Z";
    let missing = fixture.root.join("managed-unavailable");
    assert_eq!(
        fixture
            .run(Ok(secret()), time, || {
                let mut policy = ClipboardPolicy::default();
                policy.backup.directory = Some(missing.clone());
                policy
            })
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!missing.exists());
    assert!(!fixture.folder.join(CURRENT_NAME).exists());

    let checks = Cell::new(0);
    assert!(matches!(fixture.run(Ok(secret()), time, || {
        let mut policy = ClipboardPolicy::default();
        if checks.replace(checks.get() + 1) != 0 {
            policy.backup.interval = Some(Interval::Off);
        }
        policy
    }).unwrap(), AutoOutcome::Waiting(status) if status.contains("managed backup schedule changed")));
    assert_eq!(checks.get(), 2);
    assert!(!fixture.folder.join(CURRENT_NAME).exists());
    assert!(
        preferences::load(&fixture.root)
            .unwrap()
            .1
            .last_auto
            .is_none()
    );
}

#[test]
fn locked_vault_failed_write_and_concurrent_instance_do_not_record_success() {
    let fixture = Fixture::new();
    let time = "2026-03-07T15:22:00Z";
    assert!(matches!(
        fixture.run(Err("Test vault is locked."), time, ClipboardPolicy::default).unwrap(),
        AutoOutcome::Waiting(status) if status == "Test vault is locked."
    ));
    let active = Operation::acquire(&fixture.root).unwrap();
    assert_eq!(
        fixture
            .run(Ok(secret()), time, ClipboardPolicy::default)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        preferences::save(
            &fixture.root,
            &fixture.preferences,
            fixture.preferences.clone()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(active);
    let current = fixture.folder.join(CURRENT_NAME);
    fs::write(&current, b"unrelated data").unwrap();
    assert!(
        fixture
            .run(Ok(secret()), time, ClipboardPolicy::default)
            .is_err()
    );
    assert_eq!(fs::read(current).unwrap(), b"unrelated data");
    assert_eq!(
        preferences::load(&fixture.root).unwrap().1,
        Progress::default()
    );
}
