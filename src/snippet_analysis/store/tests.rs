use std::fs;

use super::*;
use crate::snippet_analysis::model::{Analysis, Finding, Provenance, Source};
use crate::snippets::{Library, tests::Profile};

fn setup() -> (Profile, Store) {
    let profile = Profile::new();
    let store = Store::new(fs::canonicalize(&profile.0).unwrap());
    (profile, store)
}

fn record(text: &str) -> Record {
    Record::new(
        Purpose::Analysis,
        None,
        Source::new(text, "Bash", "de", "en", "Health check").unwrap(),
        Provenance {
            profile_id: 12,
            profile_name: "Test profile".into(),
            model: "Test model".into(),
            duration_millis: 250,
            ..Provenance::default()
        },
        Analysis {
            summary: "Use safer arguments.".into(),
            improvements: vec![Finding {
                id: "I1".into(),
                title: "Quote the argument".into(),
                line: Some(1),
                ..Finding::default()
            }],
            ..Analysis::default()
        },
    )
    .unwrap()
}

#[test]
fn history_round_trip_deduplicates_source_blobs_and_retains_full_hash() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let first = record("first\r\nsecond\n");
    let second = record("first\r\nsecond\n");
    assert!(store.load(subject).unwrap().records.is_empty());
    assert!(!store.profile.join(DIRECTORY_NAME).exists());

    store.append(subject, first.clone()).unwrap();
    let expected = store.append(subject, second.clone()).unwrap();
    let loaded = Store::new(store.profile.clone()).load(subject).unwrap();
    assert_eq!(loaded, expected);
    assert_eq!(loaded.revision, 2);
    assert_eq!(loaded.current(), Some(&second));
    assert_eq!(loaded.records[1], first);
    assert_eq!(second.source.line_count, 2);
    assert!(Arc::ptr_eq(
        loaded.records[0].source.content.as_ref().unwrap(),
        loaded.records[1].source.content.as_ref().unwrap(),
    ));
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    let bytes = fs::read(path).unwrap();
    let document: Document = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document.blobs.len(), 1);
    assert!(!format!("{second:?}").contains("first\\r\\nsecond"));

    let oversized = "é".repeat(super::super::model::DEFAULT_CONTENT_BYTES / 2 + 1);
    let third = record(&oversized);
    assert!(third.source.content.is_none());
    assert!(third.source.content_truncated);
    assert_eq!(third.source.sha256, content_hash(&oversized).unwrap());
    store.append(subject, third.clone()).unwrap();
    assert_eq!(store.load(subject).unwrap().current(), Some(&third));
}

#[test]
fn folder_history_survives_rename_and_move_and_is_separate_from_snippet_history() {
    let (profile, store) = setup();
    let mut library = Library::open(profile.path()).unwrap();
    let folder = library.create_folder(None, "Ops").unwrap();
    let destination = library.create_folder(None, "Infrastructure").unwrap();
    let subject = Subject::Folder(folder);
    let first = record("echo folder");
    store.append(subject, first.clone()).unwrap();
    library.rename_folder(folder, "Production").unwrap();
    library.move_folder(folder, Some(destination)).unwrap();
    assert_eq!(
        library.folder(folder).unwrap().path,
        "Infrastructure/Production"
    );

    let folder_key = file_name(subject).unwrap();
    let id: SnippetId = folder_key
        .strip_prefix("folder-")
        .unwrap()
        .strip_suffix(".json")
        .unwrap()
        .to_owned()
        .try_into()
        .unwrap();
    assert!(store.load(Subject::Snippet(id)).unwrap().records.is_empty());
    store
        .append(Subject::Snippet(id), record("echo snippet"))
        .unwrap();
    assert_eq!(store.load(subject).unwrap().current(), Some(&first));
}

#[test]
fn history_mutations_use_latest_revision_preserve_pins_and_allow_idempotent_retry() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let retained = record("oldest");
    let pinned = store.append(subject, retained.clone()).unwrap();
    assert_eq!(pinned.revision, 1);
    let pinned = store.set_pinned(subject, retained.id, true).unwrap();
    assert_eq!(pinned.revision, 2);

    let writes: Vec<_> = (0..RETENTION + 2)
        .map(|index| {
            let store = store.clone();
            std::thread::spawn(move || {
                store
                    .append(subject, record(&format!("run {index}")))
                    .unwrap()
            })
        })
        .collect();
    for write in writes {
        write.join().unwrap();
    }
    let history = store.load(subject).unwrap();
    assert_eq!(history.records.len(), RETENTION);
    assert_eq!(history.revision, (RETENTION + 4) as u64);
    assert!(history.records.last().unwrap().pinned);
    assert_eq!(history.records.last().unwrap().id, retained.id);
    assert_eq!(store.append(subject, retained.clone()).unwrap(), history);
    let updated = store.remove(subject, retained.id).unwrap();
    assert_eq!(updated.revision, history.revision + 1);
    assert!(
        !updated
            .records
            .iter()
            .any(|record| record.id == retained.id)
    );
    assert_eq!(store.remove(subject, retained.id).unwrap(), updated);
}

#[test]
fn clearing_history_removes_blobs_and_rejects_late_analysis_writes() {
    use std::fs::TryLockError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::sync_channel;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let other = Subject::Snippet(SnippetId::new().unwrap());
    store.clear(subject).unwrap();
    assert!(!store.profile.join(DIRECTORY_NAME).exists());
    let empty_directory = store.directory(true).unwrap();
    store.clear(subject).unwrap();
    assert!(!empty_directory.has_entry(LOCK_NAME).unwrap());
    drop(empty_directory);

    store
        .append(subject, record("echo source to delete"))
        .unwrap();
    store.append(other, record("echo independent")).unwrap();

    let current = Arc::new(AtomicBool::new(true));
    let (guard_entered, wait_for_guard) = sync_channel(0);
    let (resume, wait_for_resume) = sync_channel(0);
    let append = {
        let store = store.clone();
        let current = Arc::clone(&current);
        std::thread::spawn(move || {
            store.append_while(subject, record("echo queued"), || {
                let still_current = current.load(Ordering::Acquire);
                guard_entered.send(()).unwrap();
                wait_for_resume.recv().unwrap();
                still_current
            })
        })
    };
    wait_for_guard.recv().unwrap();
    let competing = store
        .directory(false)
        .unwrap()
        .open_lock(LOCK_NAME)
        .unwrap();
    assert!(matches!(
        competing.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    drop(competing);

    current.store(false, Ordering::Release);
    resume.send(()).unwrap();
    store.clear(subject).unwrap();
    assert!(append.join().unwrap().is_ok());

    let error = store
        .append_while(subject, record("echo late"), || {
            current.load(Ordering::Acquire)
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    assert!(!path.exists());
    assert!(store.load(subject).unwrap().records.is_empty());
    assert_eq!(store.load(other).unwrap().records.len(), 1);
    store.clear(subject).unwrap();
    assert!(!path.exists());
}

#[test]
fn corrupt_future_or_foreign_histories_are_never_replaced() {
    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    store.append(subject, record("echo original")).unwrap();
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    let original = fs::read(&path).unwrap();
    let mut foreign: Document = serde_json::from_slice(&original).unwrap();
    foreign.history.subject = Subject::Snippet(SnippetId::new().unwrap());
    let mut future: Document = serde_json::from_slice(&original).unwrap();
    future.schema_version += 1;
    let mut corrupt: Document = serde_json::from_slice(&original).unwrap();
    *corrupt.blobs.values_mut().next().unwrap() = Arc::from("corrupt source");
    for bytes in [
        b"{invalid json".to_vec(),
        serde_json::to_vec(&foreign).unwrap(),
        serde_json::to_vec(&future).unwrap(),
        serde_json::to_vec(&corrupt).unwrap(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(store.load(subject).is_err());
        assert!(store.append(subject, record("echo new")).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn history_rejects_symlinks_and_hard_links() {
    use std::os::unix::fs::symlink;

    let (_profile, store) = setup();
    let subject = Subject::Snippet(SnippetId::new().unwrap());
    let directory = store.directory(true).unwrap();
    let external = store.profile.join("external.txt");
    let path = store
        .profile
        .join(DIRECTORY_NAME)
        .join(file_name(subject).unwrap());
    drop(directory);
    fs::write(&external, b"outside").unwrap();

    symlink(&external, &path).unwrap();
    assert!(store.load(subject).is_err());
    assert!(store.append(subject, record("echo new")).is_err());
    assert!(fs::symlink_metadata(&path).unwrap().is_symlink());

    fs::remove_file(&path).unwrap();
    fs::hard_link(&external, &path).unwrap();
    assert!(store.load(subject).is_err());
    assert!(store.append(subject, record("echo new")).is_err());
    assert_eq!(fs::read(external).unwrap(), b"outside");
}
