use std::fs;
use std::os::unix::fs::symlink;

use crate::snippets::tests::Profile;

use super::*;

fn select(root: &Path, names: &[&str]) -> Vec<Entry> {
    let listing = files::list_local(root).unwrap();
    names
        .iter()
        .map(|name| {
            listing
                .entries
                .iter()
                .find(|entry| entry.name == EntryName::Local(OsString::from(*name)))
                .unwrap()
                .clone()
        })
        .collect()
}

#[test]
fn local_recursive_ownership_reviews_exact_postorder_and_skips_links() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let folder = root.join("folder");
    let nested = folder.join("child");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(&nested).unwrap();
    // APFS accepts the Unicode fixture. Other Unix hosts also exercise raw bytes.
    #[cfg(target_os = "macos")]
    let (raw_name, expected_path) = (OsString::from("file-猫"), r"folder/child/file-\xe7\x8c\xab");
    #[cfg(not(target_os = "macos"))]
    let (raw_name, expected_path) = {
        use std::os::unix::ffi::OsStringExt as _;

        (
            OsString::from_vec(b"file-\xff".to_vec()),
            r"folder/child/file-\xff",
        )
    };
    fs::write(nested.join(raw_name), b"content").unwrap();
    let outside = root.join("outside");
    fs::write(&outside, b"outside").unwrap();
    symlink(&outside, folder.join("symlink")).unwrap();
    fs::hard_link(&outside, folder.join("hardlink")).unwrap();
    let owner = fs::metadata(&folder).unwrap().uid();
    let plan = prepare(
        root.clone(),
        &select(&root, &["folder"]),
        principals::Selection::from_ids(Some(owner), None),
        true,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(plan.skipped(), 2);
    let rows = plan.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].path, expected_path);
    assert!(!rows[0].directory);
    assert_eq!(rows[1].path, "folder/child");
    assert!(rows[1].directory);
    assert_eq!(rows[2].path, "folder");
    let Reply::Changed { note, .. } = apply(&plan, || Ok(())).unwrap() else {
        panic!("expected a completed local ownership review");
    };
    assert!(note.contains("0 changed, 3 already matched"));
    let used = apply(&plan, || Ok(())).unwrap_err();
    assert!(used.message.contains("already used"));
    assert!(
        prepare(
            root.clone(),
            &select(&root, &["outside"]),
            principals::Selection::from_ids(Some(owner), None),
            false,
            || Ok(()),
        )
        .is_err()
    );
}

#[test]
fn local_ownership_denies_a_stale_batch_before_touching_earlier_entries() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let first = root.join("first");
    let later = root.join("later");
    fs::write(&first, b"fixture").unwrap();
    fs::write(&later, b"fixture").unwrap();
    let first_stamp = LocalStamp::capture(&fs::metadata(&first).unwrap());
    let owner = fs::metadata(&first).unwrap().uid();
    let requested_owner = u32::from(owner == 0);
    let plan = prepare(
        root.clone(),
        &select(&root, &["first", "later"]),
        principals::Selection::from_ids(Some(requested_owner), None),
        false,
        || Ok(()),
    )
    .unwrap();
    fs::rename(&later, root.join("saved-inode")).unwrap();
    fs::write(&later, b"fixture").unwrap();
    let error = apply(&plan, || Ok(())).unwrap_err();
    assert!(error.message.contains("selected local entries changed"));
    assert_eq!(
        LocalStamp::capture(&fs::metadata(first).unwrap()),
        first_stamp
    );
}

#[test]
fn local_recursive_ownership_requires_a_new_review_when_the_tree_grows() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let folder = root.join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("existing"), b"fixture").unwrap();
    let group = fs::metadata(&folder).unwrap().gid();
    let plan = prepare(
        root.clone(),
        &select(&root, &["folder"]),
        principals::Selection::from_ids(None, Some(group)),
        true,
        || Ok(()),
    )
    .unwrap();
    fs::write(folder.join("new"), b"fixture").unwrap();
    assert!(apply(&plan, || Ok(())).is_err());
}

#[test]
fn local_recursive_permissions_verify_zero_modes_without_reopening_entries() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let folder = root.join("folder");
    let child = folder.join("child");
    let path = child.join("fixture");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(&child).unwrap();
    fs::write(&path, b"fixture").unwrap();
    fs::set_permissions(&child, Permissions::from_mode(0o1700)).unwrap();
    let handles = [&folder, &child, &path].map(|path| File::open(path).unwrap());
    let original = handles.each_ref().map(|file| file.metadata().unwrap());
    let selection = principals::Selection::parse_with_mode("", "", "000").unwrap();
    let plan = prepare(
        root.clone(),
        &select(&root, &["folder"]),
        selection,
        true,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(plan.rows()[1].mode, 0o1700);
    let result = apply(&plan, || Ok(()));
    let current = handles.each_ref().map(|file| file.metadata());
    // Restore access before assertions so an interrupted fixture can be removed.
    for (file, before) in handles.iter().zip(&original) {
        file.set_permissions(before.permissions()).unwrap();
    }
    let Reply::Changed { note, .. } = result.unwrap() else {
        panic!("expected completed recursive permissions");
    };
    assert!(note.contains("3 changed, 0 already matched"), "{note}");
    for (before, after) in original.into_iter().zip(current) {
        let after = after.unwrap();
        assert_eq!(after.mode() & PERMISSION_BITS, 0);
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
    }
}

#[test]
#[ignore = "requires KORTTY_TEST_OWNERSHIP_GID to name an authorized supplemental group"]
fn local_ownership_changes_the_group_and_reads_back_the_same_inode() {
    let group: u32 = std::env::var("KORTTY_TEST_OWNERSHIP_GID")
        .unwrap()
        .parse()
        .unwrap();
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let path = root.join("fixture");
    fs::write(&path, b"fixture").unwrap();
    fs::set_permissions(&path, Permissions::from_mode(0o600)).unwrap();
    let before = fs::metadata(&path).unwrap();
    assert_ne!(before.gid(), group, "the test must exercise a group change");
    let group_name = std::env::var("KORTTY_TEST_OWNERSHIP_GROUP").ok();
    let group_input = group_name.clone().unwrap_or_else(|| group.to_string());
    let principals = principals::Selection::parse_with_mode("", &group_input, "640").unwrap();
    let plan = prepare(
        root.clone(),
        &select(&root, &["fixture"]),
        principals,
        false,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(plan.uids(), (None, Some(group)));
    if let Some(name) = group_name {
        assert_eq!(
            plan.principals()
                .label(principals::Kind::Group, Some(group)),
            format!("{name} → {group}")
        );
        eprintln!("Local ownership fixture group {name} resolved to GID {group}");
    }
    let Reply::Changed { note, .. } = apply(&plan, || Ok(())).unwrap() else {
        panic!("expected a completed local ownership change");
    };
    assert!(note.contains("1 changed, 0 already matched"));
    let after = fs::metadata(&path).unwrap();
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!((after.uid(), after.gid()), (before.uid(), group));
    assert_eq!(after.mode() & PERMISSION_BITS, 0o640);

    let restore = prepare(
        root.clone(),
        &select(&root, &["fixture"]),
        principals::Selection::from_ids(None, Some(before.gid())),
        false,
        || Ok(()),
    )
    .unwrap();
    apply(&restore, || Ok(())).unwrap();
    let restored = fs::metadata(path).unwrap();
    assert_eq!(restored.gid(), before.gid());
    assert_eq!(restored.mode() & PERMISSION_BITS, 0o640);
}
