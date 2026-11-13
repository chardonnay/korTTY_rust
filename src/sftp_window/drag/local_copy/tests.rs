use crate::snippets::tests::Profile;

use super::*;

#[test]
fn sftp_local_drop_copies_files_and_folders_with_keep_both_names() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let from = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(from.join("folder/subfolder")).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(from.join("report.txt"), b"dropped file").unwrap();
    fs::write(from.join("folder/subfolder/data.txt"), b"nested file").unwrap();
    fs::write(target.join("report.txt"), b"existing file").unwrap();
    fs::create_dir(target.join("folder")).unwrap();
    let prepared = Prepared::new(
        target.clone(),
        vec![from.join("report.txt"), from.join("folder")],
    )
    .unwrap();
    let report = copy(&prepared, 0, &|| false);
    assert_eq!(
        (report.copied, report.skipped, report.failed),
        (2, 0, 0),
        "{:?}",
        report.first_failure
    );
    assert_eq!(
        fs::read(target.join("report.txt")).unwrap(),
        b"existing file"
    );
    assert_eq!(
        fs::read(target.join("report (2).txt")).unwrap(),
        b"dropped file"
    );
    assert_eq!(
        fs::read(target.join("folder (2)/subfolder/data.txt")).unwrap(),
        b"nested file"
    );
    assert_eq!(fs::read_dir(&target).unwrap().count(), 4);
    assert_eq!(fs::read_dir(target.join("folder")).unwrap().count(), 0);
}

#[test]
fn sftp_local_drop_skips_same_folder_and_refuses_own_descendant() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let source = root.join("folder");
    let target = source.join("target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("already.txt"), b"same folder").unwrap();
    let prepared = Prepared::new(target.clone(), vec![target.join("already.txt"), source]).unwrap();
    let report = copy(&prepared, 0, &|| false);
    assert_eq!((report.copied, report.skipped, report.failed), (0, 1, 1));
    assert!(
        report
            .first_failure
            .unwrap()
            .contains("cannot be copied into itself")
    );
    assert_eq!(fs::read_dir(target).unwrap().count(), 1);
}

#[test]
fn sftp_local_drop_preserves_a_replaced_destination_and_cleans_cancellation() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("target");
    let source = root.join("source.txt");
    fs::create_dir(&target).unwrap();
    fs::write(&source, vec![42; 200_000]).unwrap();
    let prepared = Prepared::new(target.clone(), vec![source.clone()]).unwrap();
    fs::rename(&target, root.join("old-target")).unwrap();
    fs::create_dir(&target).unwrap();
    let report = copy(&prepared, 0, &|| false);
    assert_eq!((report.copied, report.failed), (0, 1));
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    assert_eq!(fs::read_dir(root.join("old-target")).unwrap().count(), 0);

    let prepared = Prepared::new(target.clone(), vec![source]).unwrap();
    let stopped = || {
        fs::read_dir(&target).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kortty-")
        })
    };
    let report = copy(&prepared, 0, &stopped);
    assert_eq!((report.copied, report.failed), (0, 0));
    assert!(report.cancelled);
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn sftp_local_drop_detects_nested_source_changes_before_publish() {
    let fixture = Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let source = root.join("source");
    let leaf = source.join("nested/report.txt");
    let destination = root.join("destination");
    fs::create_dir_all(leaf.parent().unwrap()).unwrap();
    fs::write(&leaf, b"original").unwrap();
    let opened = Directory::open_path(&source).unwrap();
    let mut cleanup_failed = false;
    let mut stage = Stage::create(&destination, &mut cleanup_failed).unwrap();
    let witness = copy_directory(
        &opened,
        stage.root(),
        &stage,
        0,
        &mut Budget::default(),
        &|| false,
    )
    .unwrap();
    witness.verify(&opened, &|| false).unwrap();
    fs::write(&leaf, b"changed after staging").unwrap();
    assert!(witness.verify(&opened, &|| false).is_err());
    stage.cleanup().unwrap();
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}
