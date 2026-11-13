use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use super::*;

fn snapshot(file: &File) -> (u32, u32, u32) {
    let metadata = file.metadata().unwrap();
    (metadata.mode() & 0o7777, metadata.uid(), metadata.gid())
}

fn require_native_access(expected: &File, actual: &File) {
    assert_eq!(snapshot(actual), snapshot(expected));
    #[cfg(target_os = "linux")]
    for name in if expected.metadata().unwrap().is_dir() {
        &["system.posix_acl_access", "system.posix_acl_default"][..]
    } else {
        &["system.posix_acl_access"][..]
    } {
        let read = |file: &File| {
            let mut bytes = vec![0; 64 * 1024];
            match rustix::fs::fgetxattr(file, *name, bytes.as_mut_slice()) {
                Ok(size) => {
                    bytes.truncate(size);
                    Ok(bytes)
                }
                Err(error) => Err(error),
            }
        };
        assert_eq!(read(actual), read(expected), "{name}");
    }
    #[cfg(target_os = "macos")]
    assert!(
        kortty_files::ExtendedAcl::capture(actual).unwrap()
            == kortty_files::ExtendedAcl::capture(expected).unwrap()
    );
}

fn compare_directory_tree_with_native_creation(root: &Path) {
    let reference = root.join("native-tree");
    fs::create_dir(&reference).unwrap();
    fs::create_dir(reference.join("nested")).unwrap();
    fs::write(reference.join("nested/leaf"), b"native bytes").unwrap();
    let parent = Directory::open_path(root).unwrap();
    let identity = Identity::directory(&parent).unwrap();
    let access = Access::capture_directory(root, &identity).unwrap();
    let outer = parent
        .create_child(std::ffi::OsStr::new("private-barrier"))
        .unwrap();
    let payload = outer
        .create_tree_payload(std::ffi::OsStr::new("payload"))
        .unwrap();
    access.apply_to_directory(&payload).unwrap();
    let nested = payload
        .create_transfer_child(std::ffi::OsStr::new("nested"))
        .unwrap();
    let mut leaf = nested
        .create_ordinary_file(std::ffi::OsStr::new("leaf"))
        .unwrap();
    leaf.write_all(b"native bytes").unwrap();
    require_native_access(&File::open(&reference).unwrap(), payload.file());
    require_native_access(
        &File::open(reference.join("nested")).unwrap(),
        nested.file(),
    );
    require_native_access(&File::open(reference.join("nested/leaf")).unwrap(), &leaf);
    assert_eq!(outer.file().metadata().unwrap().mode() & 0o077, 0);
    #[cfg(target_os = "macos")]
    kortty_files::require_no_extended_acl(outer.file()).unwrap();
    assert!(access.apply_to(&leaf).is_err());
    assert!(fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".kortty-creation-")
    }));
}

#[test]
fn ordinary_directory_tree_matches_native_creation_behind_a_private_barrier() {
    let fixture = crate::snippets::tests::Profile::new();
    compare_directory_tree_with_native_creation(&fixture.0.canonicalize().unwrap());
}

#[test]
fn ordinary_directory_capture_rejects_changed_parent_and_mismatched_entry_type() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let parent = Directory::open_path(&root).unwrap();
    let identity = Identity::directory(&parent).unwrap();
    let access = Access::capture_directory(&root, &identity).unwrap();
    let file_access = Access::capture(&root, &identity).unwrap();
    let outer = parent.create_child(std::ffi::OsStr::new("outer")).unwrap();
    assert!(file_access.apply_to_directory(&outer).is_err());
    fs::set_permissions(&root, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(access.verify_parent().is_err());
    assert!(access.apply_to_directory(&outer).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_directory_tree_preserves_setgid_and_inherited_default_acl() {
    use rustix::fs::{XattrFlags, fsetxattr};
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o2750)).unwrap();
    let parent = Directory::open_path(&root).unwrap();
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 4, 65534),
        (4, 5, u32::MAX),
        (16, 5, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend_from_slice(&tag.to_le_bytes());
        acl.extend_from_slice(&permissions.to_le_bytes());
        acl.extend_from_slice(&id.to_le_bytes());
    }
    fsetxattr(
        parent.file(),
        "system.posix_acl_default",
        &acl,
        XattrFlags::empty(),
    )
    .unwrap();
    compare_directory_tree_with_native_creation(&root);
    assert_ne!(
        fs::metadata(root.join("native-tree")).unwrap().mode() & 0o2000,
        0
    );
}

#[cfg(target_os = "macos")]
#[test]
fn ordinary_directory_tree_preserves_darwin_inheritance_through_nested_children() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,write,file_inherit,directory_inherit"
            ])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    compare_directory_tree_with_native_creation(&root);
}

fn compare_with_native_creation(root: &Path) {
    let directory = Directory::open_path(root).unwrap();
    let identity = Identity::directory(&directory).unwrap();
    let access = Access::capture(root, &identity).unwrap();
    assert!(fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".kortty-creation-")
    }));
    let reference = File::create(root.join("reference")).unwrap();
    let stage = directory
        .create_child(std::ffi::OsStr::new("private"))
        .unwrap();
    let mut payload = stage.create_file(std::ffi::OsStr::new("payload")).unwrap();
    payload.write_all(b"complete file contents").unwrap();
    payload
        .set_permissions(fs::Permissions::from_mode(0o4700))
        .unwrap();
    access.apply_to(&payload).unwrap();
    assert_eq!(snapshot(&payload), snapshot(&reference));
    #[cfg(target_os = "linux")]
    {
        let mut expected = vec![0; 64 * 1024];
        let mut actual = vec![0; 64 * 1024];
        let reference_acl = rustix::fs::fgetxattr(
            &reference,
            "system.posix_acl_access",
            expected.as_mut_slice(),
        );
        let payload_acl =
            rustix::fs::fgetxattr(&payload, "system.posix_acl_access", actual.as_mut_slice());
        match (reference_acl, payload_acl) {
            (Ok(expected_len), Ok(actual_len)) => {
                assert_eq!(&expected[..expected_len], &actual[..actual_len])
            }
            (Err(expected), Err(actual)) => assert_eq!(expected, actual),
            other => panic!("Native and completed ACL availability differs: {other:?}"),
        }
    }
    #[cfg(target_os = "macos")]
    assert!(
        kortty_files::ExtendedAcl::capture(&payload).unwrap()
            == kortty_files::ExtendedAcl::capture(&reference).unwrap()
    );
}

#[test]
fn ordinary_file_permissions_match_native_creation_and_remove_empty_probe() {
    let fixture = crate::snippets::tests::Profile::new();
    compare_with_native_creation(&fixture.0.canonicalize().unwrap());
}

#[test]
fn ordinary_capture_rejects_changed_parent_permissions_and_identity() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let path = root.join("destination");
    fs::create_dir(&path).unwrap();
    let parent = Directory::open_path(&path).unwrap();
    let identity = Identity::directory(&parent).unwrap();
    let access = Access::capture(&path, &identity).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(access.verify_parent().is_err());
    fs::rename(&path, root.join("original")).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(access.verify_parent().is_err());
    assert!(Access::capture(&path, &identity).is_err());
    assert_eq!(fs::read_dir(&path).unwrap().count(), 0);
}

#[test]
fn ordinary_creation_follows_restrictive_umask_in_an_isolated_process() {
    check_isolated_umask(
        "umask 077; exec \"$@\"",
        0o600,
        "ordinary_creation_follows_restrictive_umask_in_an_isolated_process",
    );
}

#[test]
fn ordinary_creation_follows_standard_umask_in_an_isolated_process() {
    check_isolated_umask(
        "umask 022; exec \"$@\"",
        0o644,
        "ordinary_creation_follows_standard_umask_in_an_isolated_process",
    );
}

fn check_isolated_umask(script: &str, expected_mode: u32, test_name: &str) {
    const GUARD: &str = "KORTTY_TEST_ORDINARY_CREATION_UMASK_CHILD";
    if std::env::var_os(GUARD).is_some() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        compare_with_native_creation(&root);
        assert_eq!(
            fs::metadata(root.join("reference")).unwrap().mode() & 0o777,
            expected_mode
        );
        return;
    }
    let status = std::process::Command::new("/bin/sh")
        .args(["-c", script, "kortty-ordinary-creation"])
        .arg(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(format!(
            "sftp_window::worker::ordinary_access::tests::{test_name}"
        ))
        .arg("--nocapture")
        .env(GUARD, "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_creation_preserves_default_acl_and_rejects_changed_inheritance() {
    use rustix::fs::{XattrFlags, fsetxattr};
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let parent = Directory::open_path(&root).unwrap();
    // Linux POSIX ACL xattr v2: owner, named UID, group, mask and other.
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 4, 65534),
        (4, 5, u32::MAX),
        (16, 5, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend_from_slice(&tag.to_le_bytes());
        acl.extend_from_slice(&permissions.to_le_bytes());
        acl.extend_from_slice(&id.to_le_bytes());
    }
    fsetxattr(
        parent.file(),
        "system.posix_acl_default",
        &acl,
        XattrFlags::empty(),
    )
    .unwrap();
    compare_with_native_creation(&root);
    let identity = Identity::directory(&parent).unwrap();
    let access = Access::capture(&root, &identity).unwrap();
    // Change only the named user's rights, preserving the parent identity/mode.
    acl[14..16].copy_from_slice(&2_u16.to_le_bytes());
    fsetxattr(
        parent.file(),
        "system.posix_acl_default",
        &acl,
        XattrFlags::empty(),
    )
    .unwrap();
    assert!(access.verify_parent().is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn ordinary_creation_preserves_darwin_inheritance_while_payload_staging_is_private() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,write,file_inherit,directory_inherit"
            ])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    compare_with_native_creation(&root);
    let parent = Directory::open_path(&root).unwrap();
    let private = parent
        .create_child(std::ffi::OsStr::new("another-private"))
        .unwrap();
    kortty_files::require_no_extended_acl(private.file()).unwrap();
    let payload = private
        .create_file(std::ffi::OsStr::new("payload"))
        .unwrap();
    kortty_files::require_no_extended_acl(&payload).unwrap();
    assert_eq!(payload.metadata().unwrap().mode() & 0o077, 0);
    let identity = Identity::directory(&parent).unwrap();
    let access = Access::capture(&root, &identity).unwrap();
    assert!(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow execute"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    assert!(access.verify_parent().is_err());
}
