use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{GENERIC_ALL, GENERIC_READ};
use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AddAccessAllowedAceEx, AddAccessDeniedAceEx, CONTAINER_INHERIT_ACE,
    CreateWellKnownSid, GetSecurityDescriptorOwner, INHERIT_ONLY_ACE, InitializeAcl,
    NO_PROPAGATE_INHERIT_ACE, OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION,
    WinCreatorGroupSid, WinCreatorOwnerSid, WinWorldSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_APPEND_DATA, FILE_EXECUTE, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_WRITE_DATA,
    READ_CONTROL, WRITE_DAC,
};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kortty-new-file-acl-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn compare_kernel_file_inheritance(parent: &Directory) {
    let original_descriptor = parent_descriptor(parent.file()).unwrap();
    let ordinary = parent
        .create_staging_file(OsStr::new("ordinary-file"))
        .unwrap();
    let expected = ReplacementMetadata::capture(&ordinary).unwrap();
    let access = NewFileAccess::capture(parent).unwrap();
    access.require_parent(parent).unwrap();
    let stage = parent.create_child(OsStr::new("private-stage")).unwrap();
    let payload = stage
        .create_replacement_file(OsStr::new("payload"))
        .unwrap();
    access.apply_to(&payload).unwrap();
    access.require_applied(&payload).unwrap();
    expected.require_applied(&payload).unwrap();
    access.require_parent(parent).unwrap();
    assert_eq!(
        original_descriptor,
        parent_descriptor(parent.file()).unwrap()
    );
    assert_eq!(parent.names(8).unwrap().len(), 2);
    parent
        .publish_file(&payload, OsStr::new("published"))
        .unwrap();
    expected.require_applied(&payload).unwrap();
}

fn compare_ordinary_file_inheritance(parent: &Directory, path: &std::path::Path) {
    let ordinary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join("ordinary-file"))
        .unwrap();
    let expected = ReplacementMetadata::capture(&ordinary).unwrap();
    let access = NewFileAccess::capture_ordinary(parent).unwrap();
    access.require_parent(parent).unwrap();
    let stage = parent.create_child(OsStr::new("private-stage")).unwrap();
    let payload = stage
        .create_replacement_file(OsStr::new("payload"))
        .unwrap();
    access.apply_to(&payload).unwrap();
    access.require_applied(&payload).unwrap();
    // Compare the full ordered ACE bytes and native DACL control, rather than
    // the Browser helper's normalized explicit ACE representation.
    expected.require_matches(&payload).unwrap();
    parent
        .publish_file(&payload, OsStr::new("published"))
        .unwrap();
    expected.require_matches(&payload).unwrap();
    access.require_parent(parent).unwrap();
}

#[test]
fn ordinary_file_acl_matches_native_default_token_fallback() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    require_parent_inheritance_model(&parent, false);
    compare_ordinary_file_inheritance(&parent, &scratch.0.join("parent"));
}

#[test]
fn ordinary_file_acl_matches_automatic_parent_without_inheritable_aces() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    let descriptor = parent_descriptor(parent.file()).unwrap();
    let setter = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(scratch.0.join("parent"))
        .unwrap();
    let mut acl = null_mut();
    let (mut present, mut defaulted) = (0, 0);
    // SAFETY: The bounded parent descriptor and setter handle stay live while
    // the same non-inheritable DACL is converted to the automatic model.
    unsafe {
        check(windows_sys::Win32::Security::GetSecurityDescriptorDacl(
            descriptor.as_ptr().cast_mut().cast(),
            &mut present,
            &mut acl,
            &mut defaulted,
        ))
        .unwrap();
        assert_eq!(present, 1);
        assert_eq!(
            SetSecurityInfo(
                setter.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null_mut(),
            ),
            0
        );
    }
    drop(setter);
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    require_parent_inheritance_model(&parent, true);
    compare_ordinary_file_inheritance(&parent, &scratch.0.join("parent"));
}

#[test]
fn ordinary_file_acl_preserves_native_inherited_ace_flags_and_control() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    set_propagation_acl(&scratch.0.join("parent"), &parent);
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    compare_ordinary_file_inheritance(&parent, &scratch.0.join("parent"));
}

#[test]
fn ordinary_file_acl_matches_legacy_inheritable_parent() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    set_propagation_acl_mode(&scratch.0.join("parent"), &parent, FILE_EXECUTE, false);
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    compare_ordinary_file_inheritance(&parent, &scratch.0.join("parent"));
}

fn compare_ordinary_tree_inheritance(parent: &Directory, path: &std::path::Path) {
    use std::io::Write as _;

    let reference_path = path.join("ordinary-tree");
    fs::create_dir(&reference_path).unwrap();
    fs::create_dir(reference_path.join("nested")).unwrap();
    fs::create_dir(reference_path.join("empty")).unwrap();
    fs::write(reference_path.join("root-file"), b"ordinary bytes").unwrap();
    fs::write(reference_path.join("nested/child-file"), b"nested bytes").unwrap();

    let expected = Directory::open_path(&reference_path).unwrap();
    let access = NewFileAccess::capture_ordinary_directory(parent).unwrap();
    let outer = parent.create_child(OsStr::new("private-outer")).unwrap();
    let payload = outer.create_tree_payload(OsStr::new("payload")).unwrap();
    outer.require_owner_only().unwrap();
    payload.require_owner_only().unwrap();
    assert!(
        access
            .apply_to(&fs::File::open(reference_path.join("root-file")).unwrap())
            .is_err()
    );
    assert!(
        NewFileAccess::capture_ordinary(parent)
            .unwrap()
            .apply_to_directory(&payload)
            .is_err()
    );
    access.apply_to_directory(&payload).unwrap();
    access.require_applied_directory(&payload).unwrap();
    ReplacementMetadata::capture_directory(&expected)
        .unwrap()
        .require_matches_directory(&payload)
        .unwrap();

    let nested = payload.create_transfer_child(OsStr::new("nested")).unwrap();
    let empty = payload.create_transfer_child(OsStr::new("empty")).unwrap();
    let mut root_file = payload
        .create_ordinary_file(OsStr::new("root-file"))
        .unwrap();
    root_file.write_all(b"ordinary bytes").unwrap();
    let mut child_file = nested
        .create_ordinary_file(OsStr::new("child-file"))
        .unwrap();
    child_file.write_all(b"nested bytes").unwrap();
    assert!(
        payload
            .create_ordinary_file(OsStr::new("root-file"))
            .is_err()
    );
    assert!(payload.create_transfer_child(OsStr::new("nested")).is_err());
    for (relative, actual) in [("nested", &nested), ("empty", &empty)] {
        let ordinary = Directory::open_path(&reference_path.join(relative)).unwrap();
        ReplacementMetadata::capture_directory(&ordinary)
            .unwrap()
            .require_matches_directory(actual)
            .unwrap();
    }
    for (relative, actual) in [
        ("root-file", &root_file),
        ("nested/child-file", &child_file),
    ] {
        let ordinary = fs::File::open(reference_path.join(relative)).unwrap();
        ReplacementMetadata::capture(&ordinary)
            .unwrap()
            .require_matches(actual)
            .unwrap();
    }
    // Ordinary descendant ACLs do not expose data through the private outer root.
    outer.require_owner_only().unwrap();
    assert!(!path.join("published-tree").exists());
    drop(child_file);
    drop(root_file);
    drop(empty);
    drop(nested);
    parent
        .publish(&payload, OsStr::new("published-tree"))
        .unwrap();
    access.require_applied_directory(&payload).unwrap();
    assert_eq!(
        fs::read(path.join("published-tree/root-file")).unwrap(),
        b"ordinary bytes"
    );
    assert_eq!(
        fs::read(path.join("published-tree/nested/child-file")).unwrap(),
        b"nested bytes"
    );
    assert!(!path.join("private-outer/payload").exists());
}

#[test]
fn ordinary_tree_acl_matches_native_default_token_inheritance() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    drop(parent);
    let path = scratch.0.join("parent");
    let parent = Directory::open_path(&path).unwrap();
    compare_ordinary_tree_inheritance(&parent, &path);
}

#[test]
fn ordinary_tree_acl_matches_native_automatic_container_and_file_propagation() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    let path = scratch.0.join("parent");
    set_propagation_acl_with_container_owner(&path, &parent, FILE_EXECUTE, true, true);
    drop(parent);
    let parent = Directory::open_path(&path).unwrap();
    require_parent_inheritance_model(&parent, true);
    compare_ordinary_tree_inheritance(&parent, &path);
}

#[test]
fn ordinary_tree_acl_matches_native_legacy_container_and_file_propagation() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    let path = scratch.0.join("parent");
    set_propagation_acl_with_container_owner(&path, &parent, FILE_EXECUTE, false, true);
    drop(parent);
    let parent = Directory::open_path(&path).unwrap();
    require_parent_inheritance_model(&parent, false);
    compare_ordinary_tree_inheritance(&parent, &path);
}

#[test]
fn ordinary_file_acl_finalizes_denied_contents_through_the_held_handle() {
    use std::io::{Read as _, Seek as _, Write as _};

    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    set_propagation_acl_with_denial(
        &scratch.0.join("parent"),
        &parent,
        FILE_READ_DATA | FILE_WRITE_DATA | FILE_APPEND_DATA,
    );
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    let ordinary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES | DELETE)
        .open(scratch.0.join("parent/ordinary-file"))
        .unwrap();
    let expected = ReplacementMetadata::capture(&ordinary).unwrap();
    let access = NewFileAccess::capture_ordinary(&parent).unwrap();
    let stage = parent.create_child(OsStr::new("private-stage")).unwrap();
    let mut payload = stage
        .create_replacement_file(OsStr::new("payload"))
        .unwrap();
    payload.write_all(b"complete scheduler download").unwrap();
    access.apply_to(&payload).unwrap();
    expected.require_matches(&payload).unwrap();
    parent
        .publish_file(&payload, OsStr::new("published"))
        .unwrap();
    expected.require_matches(&payload).unwrap();
    assert_eq!(
        fs::read(scratch.0.join("parent/published"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        OpenOptions::new()
            .write(true)
            .open(scratch.0.join("parent/published"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    payload.rewind().unwrap();
    let mut bytes = Vec::new();
    payload.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"complete scheduler download");
    crate::restore_private_file_access(&payload).unwrap();
    crate::tree::remove(payload).unwrap();
}

#[test]
fn new_file_acl_matches_native_default_token_fallback() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    // A protected owner-only directory has no inheritable ACEs. The native
    // operation must use the creating token's default DACL for the new file.
    compare_kernel_file_inheritance(&parent);
}

#[test]
fn new_file_acl_matches_native_creator_and_ace_propagation_rules() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    set_propagation_acl(&scratch.0.join("parent"), &parent);
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    compare_kernel_file_inheritance(&parent);
}

#[test]
fn new_file_acl_rejects_a_changed_parent_acl_and_another_directory() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    let other = root.create_child(OsStr::new("other")).unwrap();
    let access = NewFileAccess::capture(&parent).unwrap();
    let ordinary = NewFileAccess::capture_ordinary(&parent).unwrap();
    let directory = NewFileAccess::capture_ordinary_directory(&parent).unwrap();
    assert!(access.require_parent(&other).is_err());
    assert!(ordinary.require_parent(&other).is_err());
    assert!(directory.require_parent(&other).is_err());
    set_propagation_acl(&scratch.0.join("parent"), &parent);
    assert!(access.require_parent(&parent).is_err());
    assert!(ordinary.require_parent(&parent).is_err());
    assert!(directory.require_parent(&parent).is_err());
}

#[test]
fn new_transfer_directory_uses_native_parent_inheritance_and_exclusive_creation() {
    let scratch = Scratch::new();
    let root = Directory::open_path(&scratch.0).unwrap();
    let parent = root.create_child(OsStr::new("parent")).unwrap();
    set_propagation_acl(&scratch.0.join("parent"), &parent);
    drop(parent);
    let parent = Directory::open_path(&scratch.0.join("parent")).unwrap();
    let actual = parent.create_transfer_child(OsStr::new("native")).unwrap();
    fs::create_dir(scratch.0.join("parent/reference")).unwrap();
    let reference = Directory::open_path(&scratch.0.join("parent/reference")).unwrap();
    let actual = parent_descriptor(actual.file()).unwrap();
    let reference = parent_descriptor(reference.file()).unwrap();
    // SAFETY: Both descriptors came from bounded native directory queries.
    assert_eq!(
        unsafe { ReplacementMetadata::from_descriptor(&actual) }.unwrap(),
        unsafe { ReplacementMetadata::from_descriptor(&reference) }.unwrap(),
    );
    assert!(parent.create_transfer_child(OsStr::new("native")).is_err());
}

#[test]
fn new_download_checkpoint_restores_private_access_after_publication_collision() {
    use std::io::Write as _;

    let scratch = Scratch::new();
    let parent = Directory::open_path(&scratch.0).unwrap();
    let access = NewFileAccess::capture(&parent).unwrap();
    let stage = parent.create_child(OsStr::new("stage")).unwrap();
    let mut payload = stage
        .create_resumable_download_file(OsStr::new("payload"))
        .unwrap();
    payload.write_all(b"complete download").unwrap();
    Directory::require_owner_only_file(&payload).unwrap();
    assert!(
        stage
            .create_resumable_download_file(OsStr::new("payload"))
            .is_err()
    );
    // The newly created stage keeps the original private sharing restriction.
    assert!(
        stage
            .open_resumable_download_file(OsStr::new("payload"))
            .is_err()
    );
    access.apply_to(&payload).unwrap();
    access.require_applied(&payload).unwrap();
    let mut competing = parent.create_file(OsStr::new("destination")).unwrap();
    competing.write_all(b"independent arrival").unwrap();
    assert!(
        parent
            .publish_file(&payload, OsStr::new("destination"))
            .is_err()
    );
    crate::restore_private_file_access(&payload).unwrap();
    Directory::require_owner_only_file(&payload).unwrap();
    assert_eq!(
        fs::read(scratch.0.join("destination")).unwrap(),
        b"independent arrival"
    );
    crate::tree::remove(payload).unwrap();
    stage.remove().unwrap();
}

#[test]
fn legacy_download_checkpoint_reopens_with_locks_and_final_permission_access() {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

    let scratch = Scratch::new();
    let parent = Directory::open_path(&scratch.0).unwrap();
    let stage = parent.create_child(OsStr::new("stage")).unwrap();
    // This is the creator used by previously saved download checkpoints.
    let mut legacy = stage.create_read_write_file(OsStr::new("payload")).unwrap();
    legacy.write_all(b"retained prefix").unwrap();
    legacy.sync_all().unwrap();
    let identity = crate::tree::file_identity(&legacy).unwrap();
    drop(legacy);

    let mut payload = stage
        .open_resumable_download_file(OsStr::new("payload"))
        .unwrap();
    assert_eq!(crate::tree::file_identity(&payload).unwrap(), identity);
    payload.try_lock().unwrap();
    let probe = stage
        .open_resumable_download_file(OsStr::new("payload"))
        .unwrap();
    assert!(matches!(
        probe.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(probe);

    let access = NewFileAccess::capture(&parent).unwrap();
    access.apply_to(&payload).unwrap();
    access.require_applied(&payload).unwrap();
    assert!(
        stage
            .open_resumable_download_file(OsStr::new("payload"))
            .is_err()
    );
    crate::restore_private_file_access(&payload).unwrap();
    Directory::require_owner_only_file(&payload).unwrap();
    assert_eq!(crate::tree::file_identity(&payload).unwrap(), identity);
    payload.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    payload.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"retained prefix");

    let probe = stage
        .open_resumable_download_file(OsStr::new("payload"))
        .unwrap();
    assert!(matches!(
        probe.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    payload.unlock().unwrap();
    probe.try_lock().unwrap();
    probe.unlock().unwrap();
    drop(probe);
    crate::tree::remove(payload).unwrap();
    stage.remove().unwrap();
}

fn set_propagation_acl(path: &std::path::Path, parent: &Directory) {
    set_propagation_acl_with_denial(path, parent, FILE_EXECUTE);
}

fn set_propagation_acl_with_denial(path: &std::path::Path, parent: &Directory, denied: u32) {
    set_propagation_acl_mode(path, parent, denied, true);
}

fn set_propagation_acl_mode(
    path: &std::path::Path,
    parent: &Directory,
    denied: u32,
    automatic: bool,
) {
    set_propagation_acl_with_container_owner(path, parent, denied, automatic, false);
}

fn set_propagation_acl_with_container_owner(
    path: &std::path::Path,
    parent: &Directory,
    denied: u32,
    automatic: bool,
    container_owner: bool,
) {
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .unwrap();
    let descriptor = parent_descriptor(parent.file()).unwrap();
    let mut owner = null_mut();
    let mut defaulted = 0;
    let mut world = [0_u32; 17];
    let mut creator_owner = [0_u32; 17];
    let mut creator_group = [0_u32; 17];
    let mut acl = [0_u32; 256];
    let pointer = acl.as_mut_ptr().cast::<ACL>();
    // SAFETY: All SID/ACL buffers are aligned and bounded. The original parent
    // descriptor and synthetic fixture handle remain live throughout setup.
    unsafe {
        check(GetSecurityDescriptorOwner(
            descriptor.as_ptr().cast_mut().cast(),
            &mut owner,
            &mut defaulted,
        ))
        .unwrap();
        for (kind, bytes) in [
            (WinWorldSid, &mut world),
            (WinCreatorOwnerSid, &mut creator_owner),
            (WinCreatorGroupSid, &mut creator_group),
        ] {
            let mut length = size_of_val(bytes) as u32;
            check(CreateWellKnownSid(
                kind,
                null_mut(),
                bytes.as_mut_ptr().cast(),
                &mut length,
            ))
            .unwrap();
        }
        check(InitializeAcl(
            pointer,
            size_of_val(&acl) as u32,
            ACL_REVISION,
        ))
        .unwrap();
        check(AddAccessDeniedAceEx(
            pointer,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | INHERIT_ONLY_ACE | NO_PROPAGATE_INHERIT_ACE,
            denied,
            world.as_mut_ptr().cast(),
        ))
        .unwrap();
        check(AddAccessAllowedAceEx(
            pointer,
            ACL_REVISION,
            0,
            FILE_ALL_ACCESS,
            owner,
        ))
        .unwrap();
        check(AddAccessAllowedAceEx(
            pointer,
            ACL_REVISION,
            OBJECT_INHERIT_ACE
                | INHERIT_ONLY_ACE
                | if container_owner {
                    CONTAINER_INHERIT_ACE
                } else {
                    0
                },
            GENERIC_ALL,
            creator_owner.as_mut_ptr().cast(),
        ))
        .unwrap();
        check(AddAccessAllowedAceEx(
            pointer,
            ACL_REVISION,
            OBJECT_INHERIT_ACE
                | INHERIT_ONLY_ACE
                | NO_PROPAGATE_INHERIT_ACE
                | if container_owner {
                    CONTAINER_INHERIT_ACE
                } else {
                    0
                },
            GENERIC_READ,
            creator_group.as_mut_ptr().cast(),
        ))
        .unwrap();
        check(AddAccessAllowedAceEx(
            pointer,
            ACL_REVISION,
            CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            world.as_mut_ptr().cast(),
        ))
        .unwrap();
        if !automatic {
            use windows_sys::Win32::Security::{
                InitializeSecurityDescriptor, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR,
                SetKernelObjectSecurity, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
            };
            use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            let sd = std::ptr::from_mut(&mut descriptor).cast();
            check(InitializeSecurityDescriptor(
                sd,
                SECURITY_DESCRIPTOR_REVISION,
            ))
            .unwrap();
            check(SetSecurityDescriptorDacl(sd, 1, pointer, 0)).unwrap();
            check(SetSecurityDescriptorControl(
                sd,
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ))
            .unwrap();
            check(SetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                sd,
            ))
            .unwrap();
        } else {
            assert_eq!(
                SetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    pointer,
                    null_mut()
                ),
                0
            );
        }
    }
    require_parent_inheritance_model(parent, automatic);
}

fn require_parent_inheritance_model(parent: &Directory, automatic: bool) {
    use windows_sys::Win32::Security::{GetSecurityDescriptorControl, SE_DACL_AUTO_INHERITED};

    let descriptor = parent_descriptor(parent.file()).unwrap();
    let (mut control, mut revision) = (0, 0);
    // SAFETY: The descriptor came from a bounded native directory query.
    unsafe {
        check(GetSecurityDescriptorControl(
            descriptor.as_ptr().cast_mut().cast(),
            &mut control,
            &mut revision,
        ))
        .unwrap();
    }
    assert_eq!(control & SE_DACL_AUTO_INHERITED != 0, automatic);
}
