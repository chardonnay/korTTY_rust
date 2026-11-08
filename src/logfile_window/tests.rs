use std::fs::{self, File, FileTimes};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::catalog::{self, Compression, Entry, Format};
use super::document::{self, Document, ExportFormat, PAGE_LINES};
use super::zstandard;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-logfile-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path);
        fs::create_dir(fixture.folder()).unwrap();
        fixture
    }

    fn folder(&self) -> PathBuf {
        self.0.join("terminal-logs")
    }

    fn entry(&self, path: &Path) -> Entry {
        catalog::scan(&self.0, || false)
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn age(path: &Path) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(600)))
        .unwrap();
}

#[test]
fn latest_page_and_live_refresh_keep_an_unterminated_last_line() {
    let fixture = Fixture::new();
    let path = fixture.folder().join("2026-11-06-13-00-00-local_1.log");
    let mut file = File::create(&path).unwrap();
    for row in 0..PAGE_LINES + 9 {
        writeln!(file, "ordinary line {row}").unwrap();
    }
    write!(file, "NEEDLE-END").unwrap();
    file.flush().unwrap();
    let entry = fixture.entry(&path);
    assert!(entry.live);
    let document = Document::open(&entry, || false).unwrap();
    assert_eq!(document.lines(), PAGE_LINES + 10);
    assert_eq!(
        document
            .page(document.lines().saturating_sub(PAGE_LINES), || false)
            .unwrap()
            .lines
            .last()
            .unwrap(),
        "NEEDLE-END"
    );
    assert_eq!(
        document.search("needle-end", None, true, || false).unwrap(),
        Some(PAGE_LINES + 9)
    );
    write!(file, "\nAPPENDED").unwrap();
    file.flush().unwrap();
    assert_eq!(document.lines(), PAGE_LINES + 10);
    let refreshed = Document::open(&entry.refresh().unwrap(), || false).unwrap();
    assert_eq!(refreshed.lines(), PAGE_LINES + 11);
    assert_eq!(
        refreshed
            .page(refreshed.lines() - 1, || false)
            .unwrap()
            .lines,
        ["APPENDED"]
    );
}

#[test]
fn catalog_loads_additional_folders_and_guards_hold_liveness_and_badges() {
    let fixture = Fixture::new();
    let extra = fixture.0.join("Java logs");
    fs::create_dir(&extra).unwrap();
    catalog::add_folder(&fixture.0, &extra).unwrap();
    let path = extra.join("2026-10-30-07-23-10-Produktiv_SSH_4.p2.json.gz");
    fs::write(&path, b"placeholder content").unwrap();
    age(&path);
    fs::write(
        extra.join(".2026-10-30-07-23-10-Produktiv_SSH_4.p2.json.lock"),
        b"java-lock",
    )
    .unwrap();
    let live = fixture.entry(&path);
    assert!(live.live);
    assert!(live.delete().is_err());
    fs::remove_file(extra.join(".2026-10-30-07-23-10-Produktiv_SSH_4.p2.json.lock")).unwrap();

    let entry = fixture.entry(&path);
    assert_eq!(entry.slug(), "Produktiv_SSH");
    assert_eq!(entry.format(), Format::Json);
    assert_eq!(entry.compression(), Compression::Gzip);
    assert!(!entry.live);
    entry.set_hold(true).unwrap();
    assert_eq!(fixture.entry(&path).hold(), Some(true));
    assert_eq!(
        entry.delete().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    entry.set_hold(false).unwrap();
    let sidecar = extra.join(".2026-10-30-07-23-10-Produktiv_SSH_4.p2.json.badges.json");
    fs::write(&sidecar, b"{malformed").unwrap();
    assert_eq!(fixture.entry(&path).hold(), None);
    assert!(entry.delete().is_err());
    assert!(path.exists());
    fs::remove_file(sidecar).unwrap();
    entry.delete().unwrap();
    assert!(!path.exists());
}

const JAVA_JSON: &str = r#"{
  "connection": "Produktiv SSH",
  "host": "test.example",
  "entries": [
    {"timestamp":"2026-10-30 07:23:10.123","username":"test","directory":"/a&b","line":"ä <not XML>\nsecond line"},
    {"timestamp":"2026-10-30 07:23:10.456","line":"<!DOCTYPE literal"}
  ]
}
"#;

#[test]
fn gzip_java_json_exports_to_zstd_xml_and_round_trips_metadata_and_entries() {
    let fixture = Fixture::new();
    let path = fixture
        .folder()
        .join("2026-10-30-07-23-10-Produktiv_4.json.gz");
    let mut gzip =
        flate2::write::GzEncoder::new(File::create(&path).unwrap(), flate2::Compression::default());
    gzip.write_all(JAVA_JSON.as_bytes()).unwrap();
    drop(gzip.finish().unwrap());
    let entry = fixture.entry(&path);
    let xml = fixture
        .folder()
        .join("2026-10-30-07-23-10-Produktiv_5.xml.zst");
    document::export(
        &entry,
        &xml,
        ExportFormat::Convert(Format::Xml),
        Compression::Zstd,
        || false,
    )
    .unwrap();
    let decoded = Document::open(&fixture.entry(&xml), || false)
        .unwrap()
        .page(0, || false)
        .unwrap();
    assert!(
        decoded
            .lines
            .iter()
            .any(|line| line.contains("directory=\"/a&amp;b\""))
    );
    assert!(
        decoded
            .lines
            .iter()
            .any(|line| line.contains("second line"))
    );

    let text = fixture
        .folder()
        .join("2026-10-30-07-23-10-Produktiv_6.log.bz2");
    document::export(
        &fixture.entry(&xml),
        &text,
        ExportFormat::Convert(Format::Text),
        Compression::Bzip2,
        || false,
    )
    .unwrap();
    let json = fixture.0.join("round-trip.json");
    document::export(
        &fixture.entry(&text),
        &json,
        ExportFormat::Convert(Format::Json),
        Compression::None,
        || false,
    )
    .unwrap();
    let expected: serde_json::Value = serde_json::from_str(JAVA_JSON).unwrap();
    let got: serde_json::Value = serde_json::from_slice(&fs::read(json).unwrap()).unwrap();
    assert_eq!(got["connection"], expected["connection"]);
    assert_eq!(got["host"], expected["host"]);
    assert_eq!(got["entries"], expected["entries"]);
}

#[test]
fn export_never_publishes_a_partial_invalid_cancelled_or_existing_destination() {
    let fixture = Fixture::new();
    let source = fixture.folder().join("2026-10-30-07-23-10-source_1.json");
    fs::write(&source, JAVA_JSON.strip_suffix("}\n").unwrap()).unwrap();
    age(&source);
    let entry = fixture.entry(&source);
    let target = fixture.0.join("export.xml");
    let format = ExportFormat::Convert(Format::Xml);
    assert!(document::export(&entry, &target, format, Compression::None, || false).is_err());
    assert!(!target.exists());
    assert_eq!(
        document::export(
            &entry,
            &target,
            ExportFormat::Unchanged,
            Compression::None,
            || true
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(!target.exists());
    fs::write(&target, b"existing").unwrap();
    assert_eq!(
        document::export(
            &entry,
            &target,
            ExportFormat::Unchanged,
            Compression::None,
            || false
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&target).unwrap(), b"existing");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
}

#[test]
fn zstd_decodes_independent_frames_and_rejects_bad_checksums_and_windows() {
    // zstd CLI v1.5.7, generated with:
    // printf 'Java-compatible zstd fixture ä\nNEEDLE-END' | zstd -q -c
    let archive = b"\x28\xb5\x2f\xfd\x04\x58\x51\x01\x00Java-compatible zstd fixture \xc3\xa4\nNEEDLE-END\x38\xd7\xe2\x5b";
    let expected = "Java-compatible zstd fixture ä\nNEEDLE-END";
    let mut text = String::new();
    zstandard::Decoder::new(archive.as_slice(), || false)
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, expected);

    let mut large_window = *archive;
    large_window[5] = 0x70;
    assert!(
        zstandard::Decoder::new(large_window.as_slice(), || false)
            .read_to_end(&mut Vec::new())
            .is_err()
    );

    let mut corrupt = *archive;
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert!(
        zstandard::Decoder::new(corrupt.as_slice(), || false)
            .read_to_end(&mut Vec::new())
            .is_err()
    );

    let repeated = expected.as_bytes().repeat(32_768);
    let mut compressed = Vec::new();
    let mut encoder = zstandard::Encoder::new(&mut compressed);
    encoder.write_all(&repeated).unwrap();
    encoder.finish().unwrap();
    let mut recovered = Vec::new();
    zstandard::Decoder::new(compressed.as_slice(), || false)
        .read_to_end(&mut recovered)
        .unwrap();
    assert_eq!(recovered, repeated);
}

#[test]
fn catalog_rejects_a_replacement_with_the_same_reported_length_and_times() {
    let fixture = Fixture::new();
    let path = fixture.folder().join("2026-10-30-07-23-10-source_1.log");
    let moved = fixture.folder().join("moved-original");
    fs::write(&path, b"original").unwrap();
    age(&path);
    let entry = fixture.entry(&path);
    let metadata = fs::metadata(&path).unwrap();
    fs::rename(&path, &moved).unwrap();
    fs::write(&path, b"replaced").unwrap();
    let times = FileTimes::new().set_modified(metadata.modified().unwrap());
    #[cfg(windows)]
    let times = {
        use std::os::windows::fs::FileTimesExt as _;
        times.set_created(metadata.created().unwrap())
    };
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(times)
        .unwrap();

    assert!(entry.refresh().is_err());
    assert!(entry.delete().is_err());
    assert_eq!(fs::read(path).unwrap(), b"replaced");
    assert_eq!(fs::read(moved).unwrap(), b"original");
}

#[test]
fn logfile_badges_apply_java_styles_and_hold_to_every_selected_log() {
    use super::catalog::badges::{self, Badge, Style, Target};

    let fixture = Fixture::new();
    let folder = fixture.folder();
    let plain = folder.join("2026-10-30-07-23-10-source_1.log");
    let gzip = plain.with_extension("log.gz");
    let live = folder.join("2026-10-30-07-24-10-live_2.log");
    fs::write(&plain, b"original").unwrap();
    fs::write(&gzip, b"archive").unwrap();
    fs::write(&live, b"live").unwrap();
    age(&plain);
    age(&gzip);
    fixture.entry(&plain).set_hold(true).unwrap();

    let targets = [&plain, &gzip, &live]
        .map(|path| Target::from_entry(&fixture.entry(path)))
        .into();
    let prepared = badges::prepare(&fixture.0, targets, &|| false).unwrap();
    assert_eq!(prepared.selected_count, 3);
    assert_eq!(prepared.held_count, 2);
    assert!(prepared.first.iter().any(Badge::held));

    let custom = Badge {
        id: "2c659d56-38eb-4dac-b2cc-8dbeeb016085".into(),
        name: "Review Ops".into(),
        icon: "clock".into(),
        color: "#13579B".into(),
        style: Style::Flag,
    };
    let mut library = Badge::defaults().to_vec();
    library.push(custom.clone());
    let selected = [library[0].clone(), custom];
    let mut file = File::options().append(true).open(&live).unwrap();
    file.write_all(b" append").unwrap();
    drop(file);
    let outcome = prepared
        .apply(&fixture.0, &library, &selected, &|| false)
        .unwrap();
    assert_eq!(outcome.notice(), "Applied badges to 3 of 3 logfiles.");
    let stored = fs::read(fixture.0.join("terminal-log-badges.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Vec<Badge>>(&stored).unwrap(),
        library
    );
    let stored_value: serde_json::Value = serde_json::from_slice(&stored).unwrap();
    assert_eq!(stored_value[6]["style"], "FLAG");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::metadata(fixture.0.join("terminal-log-badges.json")).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
    }
    for path in [&plain, &gzip, &live] {
        assert_eq!(fixture.entry(path).badge_labels().unwrap(), selected);
    }
    assert_eq!(
        fixture.entry(&gzip).delete().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read(plain).unwrap(), b"original");
    assert_eq!(fs::read(gzip).unwrap(), b"archive");
    assert_eq!(fs::read(live).unwrap(), b"live append");
}

#[test]
fn logfile_badges_reject_stale_stores_and_replaced_logs_before_writing() {
    use super::catalog::badges::{self, Badge, Target};

    let fixture = Fixture::new();
    let folder = fixture.folder();
    let first = folder.join("2026-10-30-07-23-10-first_1.log");
    let second = folder.join("2026-10-30-07-24-10-second_2.log");
    for path in [&first, &second] {
        fs::write(path, b"original").unwrap();
        age(path);
    }
    let entries = || {
        [&first, &second]
            .map(|path| Target::from_entry(&fixture.entry(path)))
            .into()
    };
    let library = Badge::defaults();
    let library_path = fixture.0.join("terminal-log-badges.json");
    let opened = badges::prepare(&fixture.0, entries(), &|| false).unwrap();
    fixture.entry(&second).set_hold(true).unwrap();
    assert!(
        opened
            .apply(&fixture.0, &library, &library[1..2], &|| false)
            .is_err()
    );
    assert_eq!(fixture.entry(&second).hold(), Some(true));
    assert!(fixture.entry(&first).badge_labels().unwrap().is_empty());
    assert!(!library_path.exists());

    let opened = badges::prepare(&fixture.0, entries(), &|| false).unwrap();
    fs::rename(&second, folder.join("retired-original")).unwrap();
    fs::write(&second, b"replacement").unwrap();
    assert!(
        opened
            .apply(&fixture.0, &library, &library[1..2], &|| false)
            .is_err()
    );
    assert!(fixture.entry(&first).badge_labels().unwrap().is_empty());
    assert!(!library_path.exists());

    let opened = badges::prepare(&fixture.0, entries(), &|| false).unwrap();
    let other_library = serde_json::to_vec(&library[1..2]).unwrap();
    fs::write(&library_path, &other_library).unwrap();
    assert!(
        opened
            .apply(&fixture.0, &library, &library[..1], &|| false)
            .is_err()
    );
    assert_eq!(fs::read(library_path).unwrap(), other_library);
    assert!(fixture.entry(&first).badge_labels().unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn logfile_badges_reject_a_replaced_folder_even_with_the_same_logfile_inode() {
    use super::catalog::badges::{self, Badge, Target};

    let fixture = Fixture::new();
    let folder = fixture.folder();
    let filename = "2026-10-30-07-23-10-first_1.log";
    let log = folder.join(filename);
    fs::write(&log, b"original").unwrap();
    age(&log);
    let opened = badges::prepare(
        &fixture.0,
        vec![Target::from_entry(&fixture.entry(&log))],
        &|| false,
    )
    .unwrap();

    let moved = fixture.0.join("reviewed-folder");
    fs::rename(&folder, &moved).unwrap();
    fs::create_dir(&folder).unwrap();
    fs::hard_link(moved.join(filename), &log).unwrap();

    let library = Badge::defaults();
    let Err(error) = opened.apply(&fixture.0, &library, &library[..1], &|| false) else {
        panic!("badge publication accepted a different directory");
    };
    assert!(error.to_string().contains("folder changed"));
    let sidecar = format!(".{filename}.badges.json");
    assert!(!folder.join(&sidecar).exists());
    assert!(!moved.join(&sidecar).exists());
    assert!(!fixture.0.join("terminal-log-badges.json").exists());
    assert_eq!(fs::read(log).unwrap(), b"original");
}

#[test]
fn logfile_badges_absent_publication_keeps_a_racing_store() {
    use super::catalog::badges::Badge;
    use crate::private_files::Directory;

    let fixture = Fixture::new();
    let cases = [
        (fixture.0.clone(), "terminal-log-badges.json"),
        (
            fixture.folder(),
            ".2026-10-30-07-23-10-first_1.log.badges.json",
        ),
    ];
    let hold = serde_json::to_vec(&Badge::defaults()[..1]).unwrap();
    for (folder, name) in cases {
        let directory = Directory::open(&folder, false).unwrap();
        let destination = folder.join(name);
        let error = directory
            .publish_checked(
                name,
                |file| file.write_all(b"[]"),
                || {
                    assert!(!destination.exists());
                    // Install a real competing file after the staged write and absence check.
                    fs::write(&destination, &hold)
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).unwrap(), hold);
        assert!(fs::read_dir(&folder).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".stage-")
        }));
    }
}

#[test]
fn logfile_badges_selection_tracks_ranges_across_pages_and_prunes_hidden_or_replaced_rows() {
    use iced::keyboard::Modifiers;

    use super::{CATALOG_PAGE, Manager};

    let fixture = Fixture::new();
    for pid in 1..=CATALOG_PAGE + 2 {
        let name = format!("2026-10-30-07-23-10-source_{pid}.log");
        fs::write(fixture.folder().join(name), b"sample").unwrap();
    }
    let entries = || catalog::scan(&fixture.0, || false).unwrap().entries;
    let mut manager = Manager::default();
    manager.receive_entries(entries());
    let first = manager.entries[CATALOG_PAGE - 1].path.clone();
    let last = manager.entries[CATALOG_PAGE + 1].path.clone();
    manager.select(first.clone(), None);
    manager.page = 1;
    manager.modifiers = Modifiers::SHIFT;
    manager.select(last.clone(), None);
    assert_eq!(manager.selected.len(), 3);
    assert!(manager.selected.contains(&first));
    assert!(manager.selected.contains(&last));
    assert!(manager.selected().is_none());

    manager.modifiers = Modifiers::COMMAND;
    manager.select(last.clone(), None);
    assert_eq!(manager.selected.len(), 2);
    manager.select(last.clone(), Some(true));
    assert_eq!(manager.selected.len(), 3);
    let filter = last.file_name().unwrap().to_str().unwrap().to_string();
    manager.filter = filter;
    manager.reset_filter_page();
    assert_eq!(manager.selected.len(), 1);
    assert_eq!(manager.selected().unwrap().path, last);

    fs::rename(&last, fixture.folder().join("retired-original")).unwrap();
    fs::write(last, b"replacement").unwrap();
    manager.receive_entries(entries());
    assert!(manager.selected.is_empty());
}

#[test]
fn logfile_batch_delete_keeps_protected_and_replaced_files_and_refreshes_the_manager() {
    use super::{Message, Mode, Prompt, Window};

    let fixture = Fixture::new();
    let write = |slug, closed| {
        let path = fixture
            .folder()
            .join(format!("2026-10-30-07-23-10-{slug}_1.log"));
        fs::write(&path, b"original").unwrap();
        if closed {
            age(&path);
        }
        path
    };
    let first = write("first", true);
    let second = write("second", true);
    let replaced = write("replaced", true);
    let live = write("live", false);
    let held = write("held", true);
    let unreadable = write("unreadable", true);
    fixture.entry(&held).set_hold(true).unwrap();
    let badge_path = fixture.folder().join(format!(
        ".{}.badges.json",
        unreadable.file_name().unwrap().to_str().unwrap()
    ));
    fs::write(badge_path, b"invalid badge data").unwrap();

    let mut window = Window::manager();
    let _ = window.update(Message::Scanned(Ok(
        catalog::scan(&fixture.0, || false).unwrap()
    )));
    let files = [&first, &second, &replaced, &live, &held, &unreadable];
    for path in files {
        let _ = window.update(Message::ToggleSelection(path.clone(), true));
    }
    let _ = window.update(Message::Delete);
    let Some(Prompt::Delete(batch)) = &window.prompt else {
        panic!("multi-selection must open one confirmation");
    };
    assert_eq!(batch.title(), "Permanently delete 3 logfiles?");
    assert_eq!(batch.uncompressed(), 3);
    assert_eq!(
        batch.skipped(),
        "Skipped: 1 active, 1 held, 1 with unreadable badges."
    );

    let _ = window.update(Message::ClearSelection);
    let _ = window.update(Message::Dismiss);
    assert_eq!(window.update(Message::Confirm).units(), 0);
    assert!(files.iter().all(|path| path.is_file()));

    let _ = window.update(Message::Delete);
    let Some(Prompt::Delete(batch)) = window.prompt.take() else {
        panic!("dismissal must retain the selection");
    };
    fs::rename(&replaced, fixture.folder().join("retired-original")).unwrap();
    fs::write(&replaced, b"replacement").unwrap();
    age(&replaced);
    let notice = batch.run(&|| false);
    assert!(notice.contains("Deleted 2 of 3 logfiles."));
    assert!(notice.contains("Could not delete 1:"));
    assert!(notice.contains(replaced.file_name().unwrap().to_str().unwrap()));
    assert_eq!(fs::read(&replaced).unwrap(), b"replacement");
    assert!(!first.exists() && !second.exists());
    assert!([live, held, unreadable].iter().all(|path| path.is_file()));

    let _ = window.update(Message::DeleteScanned {
        report: Ok(catalog::scan(&fixture.0, || false).unwrap()),
        notice: notice.clone(),
    });
    let Mode::Manager(manager) = &window.mode else {
        panic!("deletion must keep the manager open");
    };
    assert_eq!(manager.entries.len(), 4);
    assert_eq!(manager.selected.len(), 3);
    assert_eq!(window.notice, notice);
}

#[test]
fn logfile_batch_delete_cancellation_preserves_unprocessed_files() {
    use std::cell::Cell;

    use super::deletion::Batch;

    let fixture = Fixture::new();
    let files = [
        "2026-10-30-07-23-10-first_1.log",
        "2026-10-30-07-23-10-second_2.log",
    ]
    .map(|name| fixture.folder().join(name));
    for path in &files {
        fs::write(path, b"original").unwrap();
        age(path);
    }
    let entries = files.each_ref().map(|path| fixture.entry(path));
    let checks = Cell::new(0);
    let notice = Batch::plan(entries.iter()).run(&|| {
        let attempts = checks.get();
        checks.set(attempts + 1);
        attempts != 0
    });
    assert!(notice.contains("Deleted 1 of 2 logfiles."));
    assert!(notice.contains("Cancelled before deleting the remaining 1."));
    assert!(!files[0].exists());
    assert_eq!(fs::read(&files[1]).unwrap(), b"original");
}

#[test]
fn logfile_range_catalog_keeps_overlapping_earlier_logs_and_prunes_selection() {
    use super::Manager;
    use super::range::Field;

    let fixture = Fixture::new();
    let zone = jiff::tz::TimeZone::system();
    let write_log = |name: &str, modified: &str| {
        let path = fixture.folder().join(name);
        let timestamp = modified
            .parse::<jiff::civil::DateTime>()
            .unwrap()
            .to_zoned(zone.clone())
            .unwrap()
            .timestamp();
        let modified = SystemTime::from(timestamp);
        let mut file = File::create(&path).unwrap();
        file.write_all(b"fixture log").unwrap();
        file.set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        path
    };
    let early = write_log("2026-10-30-07-00-00-early_1.log", "2026-10-30 08:29:59.999");
    let overlaps = write_log(
        "2026-10-30-07-00-00-overlap_2.log",
        "2026-10-30 08:30:00.250",
    );
    let last = write_log("2026-10-30-08-30-59-last_3.log", "2026-10-30 08:30:59.999");
    let after = write_log("2026-10-30-08-31-00-after_4.log", "2026-10-30 08:31:00");
    let mut manager = Manager::default();
    manager.receive_entries(catalog::scan(&fixture.0, || false).unwrap().entries);
    manager.select(early.clone(), Some(true));
    manager.select(overlaps.clone(), Some(true));
    manager.select(after.clone(), Some(true));

    for (field, value) in [
        (Field::FromDate, "2026-10-30"),
        (Field::FromTime, "08:30"),
        (Field::ToDate, "2026-10-30"),
        (Field::ToTime, "08:30"),
    ] {
        manager.range.set(field, value.into());
    }
    manager.reset_filter_page();
    let included = manager
        .filtered()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(included, [last, overlaps.clone()]);
    assert_eq!(
        manager.selected,
        std::collections::BTreeSet::from([overlaps])
    );
}

#[test]
fn logfile_range_export_cuts_complete_entries_and_never_publishes_an_empty_cut() {
    use super::range::{Field, Fields};

    let fixture = Fixture::new();
    let path = fixture.folder().join("2026-10-30-07-00-00-source_1.log");
    fs::write(
        &path,
        concat!(
            "[2026-10-30 08:29:59.999] before\n",
            "[2026-10-30 08:30:00.250] first\n",
            "continuation\n",
            "[2026-10-30 08:30:59.999] last\n",
            "[2026-10-30 08:31:00.000] after\n",
        ),
    )
    .unwrap();
    age(&path);
    let entry = fixture.entry(&path);
    let mut fields = Fields::default();
    for (field, value) in [
        (Field::FromDate, "2026-10-30"),
        (Field::FromTime, "08:30"),
        (Field::ToDate, "2026-10-30"),
        (Field::ToTime, "08:30"),
    ] {
        fields.set(field, value.into());
    }
    let range = fields.evaluate().export().unwrap();
    let format = ExportFormat::Convert(Format::Json);
    let output = PathBuf::from(entry.export_path(format, Compression::None, range));
    assert!(
        output
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("-cut-20261030-0830-20261030-0831-export.json")
    );
    document::export_range(&entry, &output, format, Compression::None, range, || false).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    let entries = stored["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["line"], "first\ncontinuation");
    assert_eq!(entries[1]["line"], "last");

    let plain = fixture.folder().join("cut-same-format.log");
    document::export_range(
        &entry,
        &plain,
        ExportFormat::Unchanged,
        Compression::None,
        range,
        || false,
    )
    .unwrap();
    let plain = fs::read_to_string(plain).unwrap();
    assert!(plain.contains("[2026-10-30 08:30:00.250] first\ncontinuation"));
    assert!(!plain.contains("before"));
    assert!(!plain.contains("after"));

    fields.set(Field::FromTime, "08:32".into());
    fields.set(Field::ToTime, "08:32".into());
    let empty = fixture.folder().join("empty-cut.json");
    assert!(
        document::export_range(
            &entry,
            &empty,
            format,
            Compression::None,
            fields.evaluate().export().unwrap(),
            || false
        )
        .is_err()
    );
    assert!(!empty.exists());
}

#[test]
fn logfile_folder_forget_only_removes_the_manually_added_preference() {
    let fixture = Fixture::new();
    let additional = fixture.0.join("Java logs #1");
    fs::create_dir(&additional).unwrap();
    let main = fixture.folder().join("2026-10-30-07-00-00-main_1.log");
    let manual = additional.join("2026-10-30-07-00-00-manual_2.log");
    fs::write(&main, b"default").unwrap();
    fs::write(&manual, b"original").unwrap();
    catalog::add_folder(&fixture.0, &fixture.folder()).unwrap();
    catalog::add_folder(&fixture.0, &additional).unwrap();
    let entry = fixture.entry(&manual);
    assert_eq!(entry.forgettable_folder(), Some(additional.as_path()));
    assert_eq!(
        entry.checked_folder().unwrap().canonicalize().unwrap(),
        additional
    );
    assert!(fixture.entry(&main).forgettable_folder().is_none());
    assert!(catalog::forget_folder(&fixture.0, &fixture.folder()).is_err());

    fs::rename(&manual, additional.join("retired-original")).unwrap();
    fs::write(&manual, b"replacement").unwrap();
    assert!(entry.checked_folder().is_err());

    catalog::forget_folder(&fixture.0, &additional).unwrap();
    let report = catalog::scan(&fixture.0, || false).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].path, main);
    assert_eq!(fs::read(manual).unwrap(), b"replacement");
    assert_eq!(
        fs::read(additional.join("retired-original")).unwrap(),
        b"original"
    );
}
