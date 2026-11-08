use std::fs;
use std::thread;

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-ssh-key-test-{}", nonce().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn profile(&self) -> PathBuf {
        self.0.join("profile")
    }

    fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn draft(name: &str, input: KeyInput) -> KeyDraft {
    KeyDraft {
        name: name.into(),
        description: "Test fixture".into(),
        input,
        passphrase: Some("0123456789abcdef0123456789abcdef".into()),
    }
}

#[test]
fn external_and_managed_lifecycle_preserves_ids_and_original_files() {
    let scratch = Scratch::new();
    let source = scratch.source("identity", b"test-only private bytes");
    let second = scratch.source("second", b"second test-only private bytes");
    let mut catalog = Catalog::open(scratch.profile()).unwrap();
    assert!(catalog.records().is_empty());
    let ids = catalog
        .apply(vec![
            Edit::Add(draft("External", KeyInput::External(source.clone()))),
            Edit::Add(draft("Managed", KeyInput::Copy(source.clone()))),
        ])
        .unwrap()
        .added;
    assert_eq!(ids, [1, 2]);

    let catalog_on_disk = Catalog::open(scratch.profile()).unwrap();
    assert_eq!(catalog_on_disk.records(), catalog.records());
    let external = catalog.get(1).unwrap();
    let managed = catalog.get(2).unwrap().clone();
    assert_eq!(catalog.display_path(1).unwrap(), source);
    let managed_path = catalog.display_path(2).unwrap();
    assert_ne!(managed_path, source);
    assert_eq!(
        managed_path.parent(),
        Some(scratch.profile().join(MANAGED).as_path())
    );
    assert_eq!(
        &*catalog.read_key(external).unwrap(),
        b"test-only private bytes"
    );
    assert_eq!(
        &*catalog.read_key(&managed).unwrap(),
        b"test-only private bytes"
    );
    let stored = fs::read_to_string(scratch.profile().join(CATALOG)).unwrap();
    assert!(stored.contains("0123456789abcdef0123456789abcdef"));
    assert!(!stored.contains("test-only private bytes"));

    let update = catalog
        .apply(vec![Edit::Update(
            2,
            draft("Updated", KeyInput::Copy(second.clone())),
        )])
        .unwrap();
    assert_eq!(update.retained_copies, 0);
    assert!(!managed_path.exists());
    assert!(catalog_on_disk.read_key(&managed).is_err());
    assert_eq!(catalog_on_disk.display_path(2).unwrap(), managed_path);
    let next_path = catalog.display_path(2).unwrap();
    assert_eq!(
        &*catalog.read_key(catalog.get(2).unwrap()).unwrap(),
        b"second test-only private bytes"
    );

    assert_eq!(
        catalog
            .apply(vec![Edit::Remove(1), Edit::Remove(2)])
            .unwrap()
            .retained_copies,
        0
    );
    assert!(!next_path.exists());
    assert!(source.exists() && second.exists());
    assert_eq!(
        catalog
            .apply(vec![Edit::Add(draft("New", KeyInput::External(second)))])
            .unwrap()
            .added,
        [3]
    );
}

#[test]
fn bad_edit_rolls_back_published_private_stage_and_keeps_previous_catalog() {
    let scratch = Scratch::new();
    let source = scratch.source("identity", b"test-only private bytes");
    let mut catalog = Catalog::open(scratch.profile()).unwrap();
    catalog
        .apply(vec![Edit::Add(draft(
            "Original",
            KeyInput::Copy(source.clone()),
        ))])
        .unwrap();
    let old_path = catalog.display_path(1).unwrap();

    assert!(
        catalog
            .apply(vec![
                Edit::Update(1, draft("Replacement", KeyInput::Copy(source))),
                Edit::Remove(1),
            ])
            .is_err()
    );
    assert_eq!(
        fs::read_dir(scratch.profile().join(MANAGED))
            .unwrap()
            .count(),
        1
    );
    assert!(old_path.exists());
    assert_eq!(
        Catalog::open(scratch.profile())
            .unwrap()
            .get(1)
            .unwrap()
            .name,
        "Original"
    );
}

#[test]
fn existing_managed_destination_is_never_clobbered() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.profile(), true).unwrap();
    let managed = directory.private_child(MANAGED, true).unwrap();
    let token = nonce().unwrap();
    managed.publish_copy(&token, b"first").unwrap();
    assert!(managed.publish_copy(&token, b"second").is_err());
    assert_eq!(
        &*read_key_file(managed.open_private_file(&token).unwrap()).unwrap(),
        b"first"
    );
    assert_eq!(
        fs::read_dir(scratch.profile().join(MANAGED))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn managed_key_copies_optional_public_sibling_and_retires_both_copies() {
    let scratch = Scratch::new();
    let source = scratch.source("identity", b"test-only private bytes");
    let companion = scratch.source("identity.pub", b"test-only public bytes");
    let mut catalog = Catalog::open(scratch.profile()).unwrap();
    catalog
        .apply(vec![Edit::Add(draft(
            "Managed",
            KeyInput::Copy(source.clone()),
        ))])
        .unwrap();
    let managed = catalog.display_path(1).unwrap();
    let companion_name = public_name(managed.file_name().unwrap().to_str().unwrap());
    let managed_companion = managed.with_file_name(companion_name);
    assert_eq!(
        fs::read(&managed_companion).unwrap(),
        b"test-only public bytes"
    );
    assert_eq!(
        catalog
            .apply(vec![Edit::Remove(1)])
            .unwrap()
            .retained_copies,
        0
    );
    assert!(!managed.exists() && !managed_companion.exists());
    assert_eq!(fs::read(&source).unwrap(), b"test-only private bytes");
    assert_eq!(fs::read(&companion).unwrap(), b"test-only public bytes");
}

#[test]
fn concurrent_writer_is_refused_and_stale_batch_cannot_replace_current_catalog() {
    let scratch = Scratch::new();
    let source = scratch.source("identity", b"test-only private bytes");
    let profile = scratch.profile();
    let mut first = Catalog::open(profile.clone()).unwrap();
    let directory = Directory::open(&profile, true).unwrap();
    let guard = StoreLock::acquire(directory.open_lock(LOCK).unwrap()).unwrap();
    let next_source = source.clone();
    let mut second = thread::spawn(move || {
        let mut catalog = Catalog::open(profile).unwrap();
        let error = catalog
            .apply(vec![Edit::Add(draft(
                "Second",
                KeyInput::Copy(next_source),
            ))])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        catalog
    })
    .join()
    .unwrap();
    drop(guard);

    let saved = first
        .apply(vec![Edit::Add(draft(
            "First",
            KeyInput::External(source.clone()),
        ))])
        .unwrap();
    assert_eq!(saved.added, [1]);
    assert!(
        second
            .apply(vec![Edit::Add(draft(
                "Stale",
                KeyInput::Copy(source.clone())
            ))])
            .is_err()
    );
    let mut reloaded = Catalog::open(scratch.profile()).unwrap();
    assert_eq!(reloaded.records(), first.records());
    assert_eq!(
        reloaded
            .apply(vec![Edit::Add(draft("Second", KeyInput::Copy(source)))])
            .unwrap()
            .added,
        [2]
    );
}

#[test]
fn malformed_catalog_and_invalid_key_metadata_are_rejected_without_overwriting() {
    let scratch = Scratch::new();
    let directory = Directory::open(&scratch.profile(), true).unwrap();
    let mut catalog = Catalog::open(scratch.profile()).unwrap();
    let too_large = scratch.source("oversized", &vec![0; MAX_KEY_BYTES + 1]);
    let empty = scratch.source("empty", b"");
    for source in [too_large, empty, scratch.0.clone()] {
        assert!(
            catalog
                .apply(vec![Edit::Add(draft(
                    "Invalid",
                    KeyInput::External(source)
                ))])
                .is_err()
        );
    }
    let mut invalid_credential = draft(
        "Invalid credential",
        KeyInput::Copy(scratch.source("identity", b"test bytes")),
    );
    invalid_credential.passphrase = Some("plaintext rejected".into());
    assert!(catalog.apply(vec![Edit::Add(invalid_credential)]).is_err());

    let malformed = br#"{"version":1,"next_id":2,"records":[{"id":1,"name":"unsafe","description":"","source":{"managed":"../identity"},"passphrase":null}]}"#;
    directory.replace_file(CATALOG, malformed).unwrap();
    assert!(Catalog::open(scratch.profile()).is_err());
    assert!(catalog.apply(vec![Edit::Remove(1)]).is_err());
    assert_eq!(
        fs::read(scratch.profile().join(CATALOG)).unwrap(),
        malformed
    );
}

#[cfg(unix)]
#[test]
fn symlinked_ancestors_leaves_and_fifos_are_refused() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let source = scratch.source("identity", b"test-only private bytes");
    let leaf = scratch.0.join("leaf");
    symlink(&source, &leaf).unwrap();
    let ancestor = scratch.0.join("ancestor");
    symlink(&scratch.0, &ancestor).unwrap();
    let fifo = scratch.0.join("fifo");
    #[cfg(target_os = "linux")]
    rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).unwrap();
    #[cfg(target_os = "macos")]
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut catalog = Catalog::open(scratch.profile()).unwrap();
    for path in [leaf, ancestor.join("identity"), fifo] {
        assert!(
            catalog
                .apply(vec![Edit::Add(draft("Unsafe", KeyInput::Copy(path)))])
                .is_err()
        );
    }
    symlink(&source, scratch.0.join("identity.pub")).unwrap();
    assert!(
        catalog
            .apply(vec![Edit::Add(draft(
                "Unsafe companion",
                KeyInput::Copy(source.clone())
            ))])
            .is_err()
    );
    assert!(Catalog::open(ancestor.join("profile")).is_err());
    assert_eq!(fs::read(&source).unwrap(), b"test-only private bytes");
}

#[cfg(unix)]
#[test]
fn managed_copy_rejects_broadened_permissions_hard_links_and_symlink_replacement() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let scratch = Scratch::new();
    let source = scratch.source("identity", b"test-only private bytes");
    let mut catalog = Catalog::open(scratch.profile()).unwrap();
    catalog
        .apply(vec![Edit::Add(draft(
            "Managed",
            KeyInput::Copy(source.clone()),
        ))])
        .unwrap();
    let copy = catalog.display_path(1).unwrap();
    assert_eq!(
        fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(catalog.read_key(catalog.get(1).unwrap()).is_err());
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o600)).unwrap();
    let hardlink = scratch.0.join("hardlink");
    fs::hard_link(&copy, &hardlink).unwrap();
    assert!(catalog.read_key(catalog.get(1).unwrap()).is_err());
    fs::remove_file(&hardlink).unwrap();
    let directory = copy.parent().unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(catalog.read_key(catalog.get(1).unwrap()).is_err());
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(&copy).unwrap();
    symlink(&source, &copy).unwrap();
    assert!(catalog.read_key(catalog.get(1).unwrap()).is_err());
    let report = catalog.apply(vec![Edit::Remove(1)]).unwrap();
    assert_eq!(report.retained_copies, 1);
    assert!(catalog.records().is_empty());
    assert!(copy.is_symlink());
    assert_eq!(fs::read(&source).unwrap(), b"test-only private bytes");
}
