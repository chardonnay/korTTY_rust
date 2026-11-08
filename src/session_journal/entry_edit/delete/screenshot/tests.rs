use std::fs;
use std::path::PathBuf;

use super::*;

const TARGET: &str = "screenshots/shot.png";

struct Fixture {
    path: PathBuf,
    journal: Option<Directory>,
}

impl Fixture {
    fn create() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-entry-image-delete-{}",
            crate::private_files::nonce().unwrap()
        ));
        let journal = Directory::open(&path, true).unwrap();
        let fixture = Self {
            path,
            journal: Some(journal),
        };
        fs::create_dir(fixture.path.join(SCREENSHOTS)).unwrap();
        fixture
    }

    fn set_image(&self) {
        fs::write(self.path.join(TARGET), b"reviewed image").unwrap();
    }

    fn journal(&self) -> &Directory {
        self.journal.as_ref().unwrap()
    }

    fn plan(&self) -> Cleanup {
        let review = Review::read(self.journal(), TARGET).unwrap().unwrap();
        Cleanup::prepare(self.journal(), TARGET, std::iter::empty(), Some(&review))
            .unwrap()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.journal.take());
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn entry_delete_screenshot_removes_only_the_reviewed_image() {
    let fixture = Fixture::create();
    fixture.set_image();
    let original = fixture.path.join("screenshots/shot.orig.png");
    let log = fixture.path.join("session-log.json");
    fs::write(&original, b"original image").unwrap();
    fs::write(&log, b"retained capture log").unwrap();
    fixture.plan().remove(fixture.journal(), || Ok(())).unwrap();
    assert!(!fixture.path.join(TARGET).exists());
    assert_eq!(fs::read(original).unwrap(), b"original image");
    assert_eq!(fs::read(log).unwrap(), b"retained capture log");
    assert_eq!(
        fs::read_dir(fixture.path.join(SCREENSHOTS))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn entry_delete_screenshot_retains_shared_path_and_case_alias() {
    let fixture = Fixture::create();
    fixture.set_image();
    let review = Review::read(fixture.journal(), TARGET).unwrap().unwrap();
    assert!(
        Cleanup::prepare(
            fixture.journal(),
            TARGET,
            [TARGET].into_iter(),
            Some(&review)
        )
        .unwrap()
        .is_none()
    );
    let alias = "Screenshots/SHOT.PNG";
    let same_on_disk = fixture.path.join(alias).is_file();
    assert_eq!(
        Cleanup::prepare(
            fixture.journal(),
            TARGET,
            [alias].into_iter(),
            Some(&review)
        )
        .unwrap()
        .is_none(),
        same_on_disk
    );
    assert_eq!(
        fs::read(fixture.path.join(TARGET)).unwrap(),
        b"reviewed image"
    );
}

#[test]
fn entry_delete_screenshot_rejects_replacements_before_and_after_preparation() {
    let fixture = Fixture::create();
    fixture.set_image();
    let review = Review::read(fixture.journal(), TARGET).unwrap().unwrap();
    let screenshots = fixture.journal().subdirectory(SCREENSHOTS, false).unwrap();
    screenshots
        .replace_file("shot.png", b"after review")
        .unwrap();
    assert!(
        Cleanup::prepare(fixture.journal(), TARGET, std::iter::empty(), Some(&review)).is_err()
    );

    let cleanup = fixture.plan();
    match screenshots.replace_file("shot.png", b"after prepare") {
        Ok(()) => {
            assert!(cleanup.remove(fixture.journal(), || Ok(())).is_err());
            assert_eq!(
                fs::read(fixture.path.join(TARGET)).unwrap(),
                b"after prepare"
            );
        }
        #[cfg(windows)]
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            // Some NTFS configurations deny replacing the file while its read handle is held.
            assert_eq!(
                fs::read(fixture.path.join(TARGET)).unwrap(),
                b"after review"
            );
            cleanup.remove(fixture.journal(), || Ok(())).unwrap();
            assert!(!fixture.path.join(TARGET).exists());
        }
        Err(error) => panic!("could not attempt to replace the reviewed image: {error}"),
    }
}

#[test]
fn entry_delete_screenshot_rejects_a_new_file_and_hardlink() {
    let fixture = Fixture::create();
    assert!(Review::read(fixture.journal(), TARGET).unwrap().is_none());
    fixture.set_image();
    assert!(Cleanup::prepare(fixture.journal(), TARGET, std::iter::empty(), None).is_err());
    let cleanup = fixture.plan();
    let link = fixture.path.join("another.png");
    fs::hard_link(fixture.path.join(TARGET), &link).unwrap();
    assert!(Review::read(fixture.journal(), TARGET).is_err());
    assert!(cleanup.remove(fixture.journal(), || Ok(())).is_err());
    assert_eq!(fs::read(link).unwrap(), b"reviewed image");
    assert!(fixture.path.join(TARGET).is_file());
}

#[cfg(unix)]
#[test]
fn entry_delete_screenshot_refuses_directory_replacement_and_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::create();
    fixture.set_image();
    let cleanup = fixture.plan();
    let original = fixture.path.join("moved");
    fs::rename(fixture.path.join(SCREENSHOTS), &original).unwrap();
    symlink(&original, fixture.path.join(SCREENSHOTS)).unwrap();
    assert!(Review::read(fixture.journal(), TARGET).is_err());
    assert!(cleanup.remove(fixture.journal(), || Ok(())).is_err());
    assert_eq!(
        fs::read(original.join("shot.png")).unwrap(),
        b"reviewed image"
    );
}

#[cfg(unix)]
#[test]
fn entry_delete_screenshot_claim_never_unlinks_a_recreated_public_name() {
    use std::cell::Cell;

    let fixture = Fixture::create();
    fixture.set_image();
    let cleanup = fixture.plan();
    let calls = Cell::new(0);
    cleanup
        .remove(fixture.journal(), || {
            // Test double for source revalidation, scheduling an actual racing file creation.
            if calls.replace(calls.get() + 1) == 1 {
                fs::write(fixture.path.join(TARGET), b"racing image")?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(
        fs::read(fixture.path.join(TARGET)).unwrap(),
        b"racing image"
    );
    assert_eq!(
        fs::read_dir(fixture.path.join(SCREENSHOTS))
            .unwrap()
            .count(),
        1
    );
}
