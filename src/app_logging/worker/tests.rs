use std::fs;
use std::path::PathBuf;
use std::time::{Instant, UNIX_EPOCH};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-app-log-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn settings(&self) -> Effective {
        Effective {
            directory: self.0.join("logs"),
            retention_days: 7,
            compress: true,
            format: Format::Text,
            rotation_max_files: 0,
            rotation_total_size_mb: 0,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn archive(log: &Log, name: &str, bytes: &[u8], modified: SystemTime) {
    log.prepared
        .directory
        .publish_with(name, |file| {
            file.write_all(bytes)?;
            file.set_times(FileTimes::new().set_modified(modified))
        })
        .unwrap();
}

fn close(writer: &mut Writer) {
    let completion = writer.close();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(result) = completion.try_recv() {
            result.unwrap();
            return;
        }
        assert!(Instant::now() < deadline, "application log did not drain");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn coding_agent_input_audit_records_only_the_verb_and_byte_count() {
    for format in [Format::Text, Format::Json] {
        let scratch = Scratch::new();
        let mut settings = scratch.settings();
        settings.format = format;
        let mut log = Log::open(Prepared::new(&settings).unwrap(), now()).unwrap();
        log.append(
            Event::CodingAgentInputSent {
                prompt: true,
                bytes: 17,
            },
            now(),
        )
        .unwrap();
        drop(log);
        let line = fs::read_to_string(settings.directory.join(ACTIVE)).unwrap();
        match format {
            Format::Text => assert!(line.ends_with("Coding agent prompt sent bytes=17\n")),
            Format::Json => {
                let entry: serde_json::Value = serde_json::from_str(&line).unwrap();
                assert_eq!(entry["message"], "Coding agent prompt sent");
                assert_eq!(entry["bytes"], 17);
                assert_eq!(entry.as_object().unwrap().len(), 5);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn shutdown_unlocks_the_log_before_completion_with_an_inherited_description() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    let log = Log::open(Prepared::new(&settings).unwrap(), now()).unwrap();
    // Test double for the real descriptor a forked child holds before exec.
    let inherited = log.active.try_clone().unwrap();
    let mut writer = Writer::start_log(log).unwrap();
    let start = || Writer::start(Prepared::new(&settings).unwrap());
    assert!(matches!(start(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));

    close(&mut writer);
    let mut replacement = start().unwrap();
    assert!(matches!(start(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    drop(inherited);
    close(&mut replacement);
}

#[test]
fn daily_rotation_preserves_previous_records_and_continues_the_active_file() {
    let scratch = Scratch::new();
    let mut log = Log::open(Prepared::new(&scratch.settings()).unwrap(), now()).unwrap();
    let day = log.day;
    log.append(Event::Started, now()).unwrap();
    log.active
        .set_times(FileTimes::new().set_modified(now()))
        .unwrap();
    log.stamp = FileStamp::read(&log.active).unwrap();
    log.append(Event::WindowOpened, now() + DAY).unwrap();
    drop(log);
    let root = scratch.settings().directory;
    assert!(
        fs::read_to_string(root.join(format!("kortty.{day}.log")))
            .unwrap()
            .contains("Application started")
    );
    let active = fs::read_to_string(root.join(ACTIVE)).unwrap();
    assert!(active.contains("Application window opened"));
    assert!(!active.contains("Application started"));
}

#[test]
fn partial_append_rollback_refreshes_the_stamp_and_allows_the_next_record() {
    let scratch = Scratch::new();
    let mut log = Log::open(Prepared::new(&scratch.settings()).unwrap(), now()).unwrap();
    log.append(Event::Started, now()).unwrap();
    let length = log.active.metadata().unwrap().len();
    // Reproduce a failed write's partial file extension with the real active file.
    log.active.write_all(b"partial diagnostic").unwrap();
    assert!(log.require_current().is_err());
    log.rollback_append(length).unwrap();
    log.append(Event::Stopping, now()).unwrap();
    drop(log);
    let content = fs::read_to_string(scratch.settings().directory.join(ACTIVE)).unwrap();
    assert_eq!(content.lines().count(), 2);
    assert!(!content.contains("partial diagnostic"));
    assert!(content.contains("Application stopping"));
}

#[test]
fn maintenance_uses_modified_age_and_preserves_gzip_source_time() {
    let scratch = Scratch::new();
    let log = Log::open(Prepared::new(&scratch.settings()).unwrap(), now()).unwrap();
    archive(&log, "kortty.2026-01-01.log", b"expired", now() - DAY * 8);
    archive(
        &log,
        "kortty.2026-01-02.log",
        b"compress me\n",
        now() - DAY * 2,
    );
    archive(
        &log,
        "kortty.2026-01-03.log",
        b"exactly one day",
        now() - DAY,
    );
    log.maintain(now()).unwrap();
    let root = &log.prepared.settings.directory;
    assert!(!root.join("kortty.2026-01-01.log").exists());
    assert!(!root.join("kortty.2026-01-02.log").exists());
    let compressed = root.join("kortty.2026-01-02.log.gz");
    assert_eq!(
        fs::metadata(&compressed).unwrap().modified().unwrap(),
        now() - DAY * 2
    );
    let mut decoded = String::new();
    flate2::read::GzDecoder::new(File::open(compressed).unwrap())
        .read_to_string(&mut decoded)
        .unwrap();
    assert_eq!(decoded, "compress me\n");
    assert!(root.join("kortty.2026-01-03.log").exists());
}

#[test]
fn zero_retention_and_caps_keep_archives_and_both_caps_remove_oldest_first() {
    let scratch = Scratch::new();
    let mut settings = scratch.settings();
    settings.compress = false;
    settings.retention_days = 0;
    let mut log = Log::open(Prepared::new(&settings).unwrap(), now()).unwrap();
    let bytes = vec![b'x'; 600 * 1024];
    for (index, name) in [
        "kortty.2026-01-01.log",
        "kortty.2026-01-02.log",
        "kortty.2026-01-03.log",
    ]
    .into_iter()
    .enumerate()
    {
        archive(&log, name, &bytes, now() - DAY * (100 - index as u32));
    }
    log.maintain(now()).unwrap();
    let root = settings.directory;
    assert_eq!(fs::read_dir(&root).unwrap().count(), 4);
    log.prepared.settings.rotation_max_files = 2;
    log.maintain(now()).unwrap();
    assert!(!root.join("kortty.2026-01-01.log").exists());
    assert!(root.join("kortty.2026-01-02.log").exists());
    log.prepared.settings.rotation_total_size_mb = 1;
    log.maintain(now()).unwrap();
    assert!(!root.join("kortty.2026-01-02.log").exists());
    assert!(root.join("kortty.2026-01-03.log").exists());
}

#[test]
fn archive_collisions_preserve_existing_archive_and_active_records() {
    let scratch = Scratch::new();
    let mut log = Log::open(Prepared::new(&scratch.settings()).unwrap(), now()).unwrap();
    log.append(Event::Started, now()).unwrap();
    let name = format!("kortty.{}.log", log.day);
    archive(&log, &name, b"existing archive", now());
    assert_eq!(
        log.append(Event::Stopping, now() + DAY).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    drop(log);
    assert_eq!(
        fs::read(scratch.settings().directory.join(name)).unwrap(),
        b"existing archive"
    );
    assert!(
        fs::read_to_string(scratch.settings().directory.join(ACTIVE))
            .unwrap()
            .contains("Application started")
    );
}

#[test]
fn retry_after_archive_publication_recovers_only_an_identical_copy() {
    let scratch = Scratch::new();
    let mut log = Log::open(Prepared::new(&scratch.settings()).unwrap(), now()).unwrap();
    log.append(Event::Started, now()).unwrap();
    let name = format!("kortty.{}.log", log.day);
    log.prepared
        .directory
        .publish_with(&name, |file| {
            copy_bounded(&mut log.active.try_clone()?, file, None)
        })
        .unwrap();
    log.append(Event::Stopping, now() + DAY).unwrap();
    drop(log);
    let text = fs::read_to_string(scratch.settings().directory.join(ACTIVE)).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.contains("Application stopping"));
}

#[test]
fn gzip_collision_preserves_both_inputs_and_does_not_overwrite() {
    let scratch = Scratch::new();
    let log = Log::open(Prepared::new(&scratch.settings()).unwrap(), now()).unwrap();
    archive(&log, "kortty.2026-01-01.log", b"raw input", now() - DAY * 2);
    archive(
        &log,
        "kortty.2026-01-01.log.gz",
        b"existing compressed",
        now() - DAY * 2,
    );
    log.maintain(now()).unwrap();
    assert_eq!(
        fs::read(
            log.prepared
                .settings
                .directory
                .join("kortty.2026-01-01.log")
        )
        .unwrap(),
        b"raw input"
    );
    assert_eq!(
        fs::read(
            log.prepared
                .settings
                .directory
                .join("kortty.2026-01-01.log.gz")
        )
        .unwrap(),
        b"existing compressed"
    );
}

#[test]
fn writer_lock_refuses_a_competitor_and_shutdown_drains_all_accepted_events() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    let mut writer = Writer::start(Prepared::new(&settings).unwrap()).unwrap();
    assert!(Writer::start(Prepared::new(&settings).unwrap()).is_err());
    for event in [
        Event::Started,
        Event::WindowOpened,
        Event::SessionStarted,
        Event::SessionStopped,
        Event::WindowClosed,
        Event::PolicyRejected,
        Event::OperationFailed,
        Event::Stopping,
    ] {
        assert!(writer.record(event));
    }
    close(&mut writer);
    let text = fs::read_to_string(settings.directory.join(ACTIVE)).unwrap();
    assert_eq!(text.lines().count(), 8);
    assert!(
        text.lines()
            .last()
            .unwrap()
            .ends_with("Application stopping")
    );
    // Completion also releases the lock, so the next launch can open this directory.
    drop(Log::open(Prepared::new(&settings).unwrap(), now()).unwrap());
}

#[test]
fn in_place_configuration_is_read_before_the_next_event() {
    let scratch = Scratch::new();
    let mut settings = scratch.settings();
    let mut writer = Writer::start(Prepared::new(&settings).unwrap()).unwrap();
    settings.format = Format::Json;
    writer.configure(settings.clone());
    assert!(writer.record(Event::ConfigurationApplied));
    close(&mut writer);
    let content = fs::read_to_string(settings.directory.join(ACTIVE)).unwrap();
    let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(
        entry["message"],
        "Application logging configuration applied"
    );
}

#[test]
fn uncommitted_candidate_does_not_maintain_existing_archives() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    let log = Log::open(Prepared::new(&settings).unwrap(), now()).unwrap();
    archive(
        &log,
        "kortty.2026-01-01.log",
        b"old retained until commit",
        UNIX_EPOCH + DAY,
    );
    drop(log);
    let mut writer = Writer::start(Prepared::new(&settings).unwrap()).unwrap();
    thread::sleep(Duration::from_millis(30));
    close(&mut writer);
    assert_eq!(
        fs::read(settings.directory.join("kortty.2026-01-01.log")).unwrap(),
        b"old retained until commit"
    );
    assert!(!settings.directory.join("kortty.2026-01-01.log.gz").exists());
}

#[test]
fn replaced_reviewed_directory_is_rejected_even_with_the_same_active_hardlink() {
    let scratch = Scratch::new();
    let settings = scratch.settings();
    let mut log = Log::open(Prepared::new(&settings).unwrap(), now()).unwrap();
    log.append(Event::Started, now()).unwrap();
    let moved = scratch.0.join("original-logs");
    let renamed = fs::rename(&settings.directory, &moved);
    #[cfg(windows)]
    if renamed
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::PermissionDenied)
    {
        assert!(settings.directory.is_dir());
        assert!(!moved.exists());
        log.require_current().unwrap();
        log.append(Event::WindowOpened, now()).unwrap();
        return;
    }
    renamed.unwrap();
    drop(open_directory(&settings.directory, true).unwrap());
    fs::hard_link(moved.join(ACTIVE), settings.directory.join(ACTIVE)).unwrap();
    assert!(log.append(Event::OperationFailed, now()).is_err());
}

#[cfg(unix)]
#[test]
fn symlink_and_hardlink_active_files_are_rejected_without_touching_the_target() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let settings = scratch.settings();
    let target = scratch.0.join("unrelated");
    fs::write(&target, b"untouched").unwrap();
    let prepared = Prepared::new(&settings).unwrap();
    symlink(&target, settings.directory.join(ACTIVE)).unwrap();
    assert!(Writer::start(prepared).is_err());
    fs::remove_file(settings.directory.join(ACTIVE)).unwrap();
    fs::hard_link(&target, settings.directory.join(ACTIVE)).unwrap();
    assert!(Writer::start(Prepared::new(&settings).unwrap()).is_err());
    assert_eq!(fs::read(target).unwrap(), b"untouched");
}
