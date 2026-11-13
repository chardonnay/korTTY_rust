//! Reviewed Scheduler downloads finalize using local ordinary creation rights.

use super::*;
use crate::sftp_window::worker::sync;

#[test]
fn scheduled_download_ignores_remote_modes_and_finalizes_private_payloads() {
    for mode in [0o100000, 0o107777] {
        run(mode, false, false);
    }
}

#[test]
fn scheduled_download_keeps_late_arrivals_and_cancels_without_publication() {
    run(0o100777, true, false);
    run(0o100777, false, true);
}

#[cfg(windows)]
#[test]
fn scheduled_changed_download_binds_same_metadata_contents_before_claim_and_keeps_recovery() {
    use crate::sftp_window::scheduled::Comparison;
    use std::cell::Cell;

    const ORIGINAL: &[u8] = b"local previous!";
    const CHANGED: &[u8] = b"local competing";
    assert_eq!(ORIGINAL.len(), SOURCES[1].1.len());
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("kept.txt");
    fs::write(&target, ORIGINAL).unwrap();
    let timestamp = std::time::UNIX_EPOCH + Duration::from_secs(10);
    let set_modified = |path: &Path| {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(timestamp)
            .unwrap();
    };
    set_modified(&target);
    let access =
        kortty_files::ReplacementMetadata::capture(&fs::File::open(&target).unwrap()).unwrap();
    let comparison = || Comparison {
        local: root.clone(),
        remote: b"/remote".to_vec(),
        excludes: "",
        to: Side::Local,
        replace_changed: true,
    };
    let fixture_client = |source: &[u8], run: bool| {
        client(|bytes| {
            let files = [SOURCES[1]];
            let mut id = snapshot_sources_at(bytes, 0o100600, 1, &files);
            id = download_reviewed_file(bytes, id, source);
            id = snapshot_sources_at(bytes, 0o100600, id, &files);
            if run {
                id = snapshot_sources_at(bytes, 0o100600, id, &files);
                download_reviewed_file(bytes, id, source);
            }
        })
    };
    let preview = comparison()
        .preview(&mut fixture_client(SOURCES[1].1, false), &|| false)
        .unwrap();
    assert!(preview.paths.is_empty());
    assert_eq!(preview.replacements.len(), 1);
    assert_eq!(preview.bytes, ORIGINAL.len() as u64);
    assert_ne!(
        preview.replacements[0].source_sha256,
        preview.replacements[0].target_sha256
    );
    let claimed = Cell::new(false);
    let claim = || {
        claimed.set(true);
        Ok(())
    };
    let mut changed_source = SOURCES[1].1.to_vec();
    changed_source[0] = b'x';
    assert!(
        comparison()
            .apply(
                &mut fixture_client(&changed_source, false),
                preview.proof,
                &|| false,
                &|| false,
                claim
            )
            .is_err()
    );
    assert!(!claimed.get());

    fs::write(&target, CHANGED).unwrap();
    set_modified(&target);
    assert!(
        comparison()
            .apply(
                &mut fixture_client(SOURCES[1].1, false),
                preview.proof,
                &|| false,
                &|| false,
                claim
            )
            .is_err()
    );
    assert!(!claimed.get());

    fs::write(&target, ORIGINAL).unwrap();
    set_modified(&target);
    let preview = comparison()
        .preview(&mut fixture_client(SOURCES[1].1, false), &|| false)
        .unwrap();
    let blocked = comparison()
        .apply_authorized(
            &mut fixture_client(SOURCES[1].1, true),
            preview.proof,
            &|| false,
            &|| false,
            claim,
            &|| Err(io::Error::other("fixture denied after claim")),
        )
        .unwrap();
    assert!(claimed.get());
    assert_eq!(blocked.replaced, 0);
    assert!(
        blocked
            .error
            .unwrap()
            .contains("fixture denied after claim")
    );
    assert_eq!(fs::read(&target).unwrap(), ORIGINAL);

    claimed.set(false);
    let applied = comparison()
        .apply_authorized(
            &mut fixture_client(SOURCES[1].1, true),
            preview.proof,
            &|| false,
            &|| false,
            claim,
            &|| Ok(()),
        )
        .unwrap();
    assert!(claimed.get());
    assert!(applied.error.is_none(), "{:?}", applied.error);
    assert_eq!(applied.replaced, 1);
    assert_eq!(fs::read(&target).unwrap(), SOURCES[1].1);
    access
        .require_applied(&fs::File::open(&target).unwrap())
        .unwrap();
    let pattern = applied.recovery_prefix.unwrap();
    let (prefix, _) = pattern.split_once('?').unwrap();
    let mut retained = fs::read_dir(&root).unwrap().filter_map(|entry| {
        let entry = entry.unwrap();
        entry
            .file_name()
            .to_string_lossy()
            .starts_with(prefix)
            .then(|| entry.path())
    });
    assert_eq!(
        fs::read(retained.next().unwrap().join("kept.txt")).unwrap(),
        ORIGINAL
    );
    assert!(retained.next().is_none());
}

fn run(mode: u32, collision: bool, cancelled: bool) {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::write(root.join("kept.txt"), b"original local").unwrap();
    let reference = fs::File::create_new(root.join("native-control")).unwrap();
    let request = compare::folders::Request::new(
        [
            Location::Local(root.clone()),
            Location::Remote(b"/remote".to_vec()),
        ],
        "",
    )
    .unwrap();
    let review = compare::folders::read(
        &mut client(|bytes| {
            reviewed_snapshot_with_mode(bytes, mode);
        }),
        request,
        || false,
    )
    .unwrap();
    let plan = sync::CopyPlan::new(Arc::new(review), Side::Local).unwrap();
    let mut bytes = Vec::new();
    packet(&mut bytes, 2, 3, &[]);
    let id = reviewed_snapshot_with_mode(&mut bytes, mode);
    let id = download_reviewed_file_with_mode(&mut bytes, id, SOURCES[0].1, mode);
    let first_complete = bytes.len() as u64;
    if !collision {
        download_reviewed_file_with_mode(&mut bytes, id, SOURCES[2].1, mode);
    }
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut transport = Sftp::new(
        Box::new(ObserveNewDownload {
            bytes: Cursor::new(bytes),
            root: root.clone(),
            first_complete,
            observed: Arc::clone(&observed),
            occupy: collision,
        }) as Box<dyn Read + Send>,
        Box::new(io::sink()) as Box<dyn Write + Send>,
    )
    .unwrap();
    let claimed = std::cell::Cell::new(false);
    let applied = sync::apply_reviewed(
        &mut transport,
        &plan,
        &Progress::new(None),
        &|| cancelled,
        &|| false,
        || {
            claimed.set(true);
            Ok(())
        },
    );
    assert_eq!(claimed.get(), !cancelled);
    assert_eq!(
        observed.load(std::sync::atomic::Ordering::Relaxed),
        !cancelled
    );
    if collision || cancelled {
        assert!(applied.result.is_err());
        assert_eq!(applied.copied, 0);
        if collision {
            assert_eq!(
                fs::read(root.join("first.txt")).unwrap(),
                b"arrived before publication"
            );
        } else {
            assert!(!root.join("first.txt").exists());
        }
    } else {
        assert!(applied.result.is_ok(), "{:?}", applied.result);
        assert_eq!(applied.copied, 2);
        for (name, contents) in [SOURCES[0], SOURCES[2]] {
            let path = root.join(std::str::from_utf8(name).unwrap());
            assert_eq!(fs::read(&path).unwrap(), contents);
            assert_ne!(
                fs::metadata(&path).unwrap().modified().unwrap(),
                std::time::UNIX_EPOCH + Duration::from_secs(10)
            );
            #[cfg(windows)]
            kortty_files::ReplacementMetadata::capture(&reference)
                .unwrap()
                .require_matches(&fs::File::open(path).unwrap())
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode(),
                    reference.metadata().unwrap().permissions().mode()
                );
            }
        }
    }
    assert_eq!(fs::read(root.join("kept.txt")).unwrap(), b"original local");
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".kortty-")
    }));
}
