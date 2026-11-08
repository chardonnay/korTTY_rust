use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};

use crate::private_files::{Directory, nonce};
use crate::session_journal::export_output::Destination;

use super::*;

const ID: &str = "png-fixture";
const ROW: &str = "screenshot-row";
const REFERENCE: &str = "screenshots/shot-000007.png";

struct Fixture {
    root: PathBuf,
    journal: PathBuf,
    export: PathBuf,
    png: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-screenshot-export-{}",
            nonce().unwrap()
        ));
        let journal = root.join("journal");
        let export = root.join("export");
        fs::create_dir_all(journal.join("screenshots")).unwrap();
        fs::create_dir(&export).unwrap();
        let png = tiny_skia::Pixmap::new(2, 3).unwrap().encode_png().unwrap();
        fs::write(journal.join(REFERENCE), &png).unwrap();
        let fixture = Self {
            root,
            journal,
            export,
            png,
        };
        fixture.write_document("SCREENSHOT", REFERENCE);
        fixture
    }

    fn write_document(&self, kind: &str, reference: &str) {
        fs::write(
            self.journal.join("journal.xml"),
            format!(
                "<session-journal id=\"{ID}\" formatVersion=\"1\"><meta/><entries>+                 <entry><id>{ROW}</id><kind>{kind}</kind><createdAt>2026-11-06T14:00:00Z</createdAt>+                 <screenshotFile>{reference}</screenshotFile></entry></entries></session-journal>"
            ),
        )
        .unwrap();
    }

    fn source(&self) -> ViewerSource {
        let directory = Directory::open(&self.journal, false).unwrap();
        ViewerSource::from_open_directory(&self.journal, ID, &directory).unwrap()
    }

    fn asset(&self) -> Arc<Screenshot> {
        Target::row(self.source(), ROW.to_owned(), REFERENCE.to_owned())
            .review()
            .unwrap()
    }

    fn destination(&self, name: &str) -> Destination {
        destination(&self.export.join(name))
    }

    fn no_private_stages(&self) {
        assert!(fs::read_dir(&self.export).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kortty-journal-png-")
        }));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn destination(path: &Path) -> Destination {
    let directory = Directory::open(path.parent().unwrap(), false).unwrap();
    Destination::new(path.to_owned(), directory.identity().unwrap()).unwrap()
}

#[test]
fn journal_screenshot_export_preserves_png_bytes_and_refuses_a_new_occupant() {
    let fixture = Fixture::new();
    let asset = fixture.asset();
    assert_eq!(asset.filename(), "shot-000007.png");
    let reviewed = PngDestination::review(fixture.destination("copy.png"), &asset).unwrap();
    assert!(!reviewed.exists());
    let output = reviewed.publish(&asset, &|| false).unwrap();
    assert_eq!(output.path, fixture.export.join("copy.png"));
    assert!(output.warning.is_none());
    assert_eq!(fs::read(output.path).unwrap(), fixture.png);

    let reviewed = PngDestination::review(fixture.destination("race.png"), &asset).unwrap();
    fs::write(reviewed.path(), b"new occupant").unwrap();
    assert!(reviewed.publish(&asset, &|| false).is_err());
    assert_eq!(fs::read(reviewed.path()).unwrap(), b"new occupant");
    fixture.no_private_stages();
}

#[test]
fn journal_screenshot_export_keeps_the_displayed_binding_and_rejects_changed_source() {
    let fixture = Fixture::new();
    let displayed = Displayed::load(&fixture.source(), ROW, REFERENCE).unwrap();
    let reviewed =
        PngDestination::review(fixture.destination("copy.png"), &displayed.asset).unwrap();
    fixture.write_document("AGENT", REFERENCE);
    assert!(Target::displayed(&displayed).review().is_err());
    assert!(reviewed.publish(&displayed.asset, &|| false).is_err());
    assert!(!reviewed.path().exists());

    fixture.write_document("SCREENSHOT", REFERENCE);
    let displayed = Displayed::load(&fixture.source(), ROW, REFERENCE).unwrap();
    let reviewed =
        PngDestination::review(fixture.destination("copy.png"), &displayed.asset).unwrap();
    fs::rename(
        fixture.journal.join(REFERENCE),
        fixture.root.join("previous.png"),
    )
    .unwrap();
    fs::write(fixture.journal.join(REFERENCE), &fixture.png).unwrap();
    assert!(reviewed.publish(&displayed.asset, &|| false).is_err());
    assert!(!reviewed.path().exists());
    fixture.no_private_stages();
}

#[test]
fn journal_screenshot_export_refuses_the_source_and_a_changed_overwrite_review() {
    let fixture = Fixture::new();
    let asset = fixture.asset();
    assert!(PngDestination::review(destination(&fixture.journal.join(REFERENCE)), &asset).is_err());

    let path = fixture.export.join("copy.png");
    fs::write(&path, b"previous").unwrap();
    let reviewed = PngDestination::review(fixture.destination("copy.png"), &asset).unwrap();
    assert!(reviewed.exists());
    fs::rename(&path, fixture.root.join("old.png")).unwrap();
    fs::write(&path, b"changed").unwrap();
    assert!(reviewed.publish(&asset, &|| false).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"changed");

    let reviewed = PngDestination::review(fixture.destination("copy.png"), &asset).unwrap();
    assert!(
        reviewed
            .publish(&asset, &|| false)
            .unwrap()
            .warning
            .is_none()
    );
    assert_eq!(fs::read(&path).unwrap(), fixture.png);
    fixture.no_private_stages();
}

#[test]
fn journal_screenshot_export_retains_both_files_if_a_racer_fills_the_claimed_slot() {
    let fixture = Fixture::new();
    let asset = fixture.asset();
    let path = fixture.export.join("copy.png");
    fs::write(&path, b"approved previous").unwrap();
    let reviewed = PngDestination::review(fixture.destination("copy.png"), &asset).unwrap();
    let raced = Cell::new(false);
    // Test double for a concurrent destination writer. It acts after the actual slot claim.
    let error = reviewed
        .publish(&asset, &|| {
            if !raced.get() && !path.exists() {
                fs::write(&path, b"racing file").unwrap();
                raced.set(true);
            }
            false
        })
        .unwrap_err();
    assert!(raced.get());
    assert_eq!(fs::read(&path).unwrap(), b"racing file");
    let stages: Vec<_> = fs::read_dir(&fixture.export)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(stages.len(), 1);
    let recovered = stages[0].join(".previous/copy.png");
    assert_eq!(fs::read(&recovered).unwrap(), b"approved previous");
    assert!(
        error
            .to_string()
            .contains(&recovered.to_string_lossy().into_owned())
    );
}

#[test]
fn journal_screenshot_export_dialog_requires_explicit_replace_and_defaults_to_source_name() {
    let fixture = Fixture::new();
    let asset = fixture.asset();
    let id = window::Id::unique();
    let mut dialog = Dialog {
        origin: Origin::Viewer(window::Id::unique()),
        owner: window::Id::unique(),
        bound: fixture.source(),
        asset: None,
        browser: None,
        phase: Phase::Loading,
        ticket: 0,
    };
    let _ = dialog.update(id, Event::Loaded(Ok(Arc::clone(&asset))));
    let listing = dialog
        .browser
        .as_ref()
        .unwrap()
        .complete_listing_for_test(&fixture.export);
    let _ = dialog.update(id, Event::Browser(listing));
    assert!(matches!(dialog.phase, Phase::Choose));
    let path = fixture.export.join("shot-000007.png");
    fs::write(&path, b"previous").unwrap();
    assert!(matches!(
        dialog.update(id, Event::Browser(browser::Event::Save)),
        Action::Task(_)
    ));
    let reviewed = PngDestination::review(
        dialog.browser.as_ref().unwrap().destination().unwrap(),
        &asset,
    )
    .unwrap();
    assert_eq!(reviewed.path(), path);
    let ticket = dialog.ticket;
    assert!(matches!(
        dialog.update(id, Event::Reviewed(ticket, Ok(Arc::new(reviewed)))),
        Action::None,
    ));
    assert!(matches!(dialog.phase, Phase::Confirm(_)));
    assert_eq!(fs::read(&path).unwrap(), b"previous");
    assert!(matches!(dialog.update(id, Event::Replace), Action::Save(_)));
    assert_eq!(fs::read(&path).unwrap(), b"previous");
}
