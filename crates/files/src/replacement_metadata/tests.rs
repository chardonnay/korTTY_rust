use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Seek, Write};
use std::mem::offset_of;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL_REVISION, AddAccessAllowedAceEx, AddAccessDeniedAceEx,
    CreateWellKnownSid, GetAce, INHERITED_ACE, InitializeAcl, PROTECTED_DACL_SECURITY_INFORMATION,
    WinWorldSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_READ, FILE_READ_DATA, FILE_WRITE_EA,
};

use crate::tree::Directory;

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kortty-replacement-acl-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn directory(&self) -> Directory {
        Directory::open_path(&self.0).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn set_acl(file: &File, inherited_reader: bool) {
    set_acl_denial(file, FILE_WRITE_EA, inherited_reader);
}

fn set_acl_denial(file: &File, denied: u32, inherited_reader: bool) {
    let private = ReplacementMetadata::capture(file).unwrap();
    let mut acl = [0_u32; 128];
    let pointer = acl.as_mut_ptr().cast::<ACL>();
    let mut world = [0_u32; 17];
    let mut world_size = size_of_val(&world) as u32;
    let mut first = null_mut();
    // SAFETY: The original private ACL was validated during capture. The ACL
    // and maximum SID buffers are aligned and remain live through both calls.
    unsafe {
        check(GetAce(private.acl.as_ptr().cast(), 0, &mut first)).unwrap();
        assert!(!first.is_null());
        let owner = first
            .cast::<u8>()
            .add(offset_of!(ACCESS_ALLOWED_ACE, SidStart))
            .cast();
        check(CreateWellKnownSid(
            WinWorldSid,
            null_mut(),
            world.as_mut_ptr().cast(),
            &mut world_size,
        ))
        .unwrap();
        check(InitializeAcl(
            pointer,
            size_of_val(&acl) as u32,
            ACL_REVISION,
        ))
        .unwrap();
        check(AddAccessDeniedAceEx(
            pointer,
            ACL_REVISION,
            0,
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
        if inherited_reader {
            check(AddAccessAllowedAceEx(
                pointer,
                ACL_REVISION,
                INHERITED_ACE,
                FILE_GENERIC_READ,
                world.as_mut_ptr().cast(),
            ))
            .unwrap();
        }
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

#[test]
fn completed_download_metadata_does_not_require_content_read_access() {
    let scratch = Scratch::new();
    let parent = scratch.directory();
    let mut payload = parent
        .create_replacement_file(OsStr::new("published"))
        .unwrap();
    payload.write_all(b"complete download").unwrap();
    let identity = crate::tree::file_identity(&payload).unwrap();
    set_acl_denial(&payload, FILE_READ_DATA, false);
    assert_eq!(
        parent
            .read_child(OsStr::new("published"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied
    );
    let mut metadata = parent.open_file_metadata(OsStr::new("published")).unwrap();
    assert_eq!(crate::tree::file_identity(&metadata).unwrap(), identity);
    assert_eq!(
        metadata.metadata().unwrap().len(),
        b"complete download".len() as u64
    );
    assert!(metadata.read(&mut [0_u8; 1]).is_err());
    let listed = parent
        .open_entry_metadata(OsStr::new("published"))
        .unwrap()
        .unwrap();
    assert_eq!(crate::tree::file_identity(&listed).unwrap(), identity);
    assert!(listed.metadata().unwrap().is_file());
    drop(listed);
    let folder = parent.create_child(OsStr::new("folder")).unwrap();
    let listed = parent
        .open_entry_metadata(OsStr::new("folder"))
        .unwrap()
        .unwrap();
    assert!(listed.metadata().unwrap().is_dir());
    assert_eq!(
        crate::tree::file_identity(&listed).unwrap(),
        folder.identity().unwrap()
    );
    drop(listed);
    assert!(parent.open_file_metadata(OsStr::new("folder")).is_err());
    folder.remove().unwrap();
    crate::restore_private_file_access(&payload).unwrap();
    Directory::require_owner_only_file(&payload).unwrap();
    drop(metadata);
    crate::tree::remove(payload).unwrap();
}

#[test]
fn replacement_acl_preserves_ordered_allow_deny_entries_and_flags_before_publication() {
    let scratch = Scratch::new();
    let parent = scratch.directory();
    let source = parent
        .create_replacement_file(OsStr::new("original"))
        .unwrap();
    set_acl(&source, true);
    let snapshot = ReplacementMetadata::capture(&source).unwrap();
    snapshot.require_matches(&source).unwrap();
    let stage = parent.create_child(OsStr::new("stage")).unwrap();
    let mut payload = stage
        .create_replacement_file(OsStr::new("payload"))
        .unwrap();
    payload.write_all(b"server bytes").unwrap();
    let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    payload.set_modified(modified).unwrap();
    assert_ne!(
        ReplacementMetadata::capture(&payload).unwrap().acl,
        snapshot.acl
    );

    snapshot.apply_to(&payload).unwrap();
    let applied = ReplacementMetadata::capture(&payload).unwrap();
    assert_eq!(applied.acl, snapshot.final_acl().unwrap());
    assert_eq!(applied.control & SE_DACL_PROTECTED, 0);
    assert_eq!(payload.metadata().unwrap().modified().unwrap(), modified);
    payload.rewind().unwrap();
    let mut bytes = Vec::new();
    payload.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"server bytes");

    parent
        .publish_file(&payload, OsStr::new("published"))
        .unwrap();
    assert_eq!(
        ReplacementMetadata::capture(&payload).unwrap().acl,
        snapshot.final_acl().unwrap()
    );
    assert_eq!(
        fs::read(scratch.0.join("published")).unwrap(),
        b"server bytes"
    );
}

#[test]
fn replacement_acl_rejects_reviewed_acl_changes_on_the_same_file_handle() {
    let scratch = Scratch::new();
    let parent = scratch.directory();
    let source = parent
        .create_replacement_file(OsStr::new("original"))
        .unwrap();
    let snapshot = ReplacementMetadata::capture(&source).unwrap();
    let identity = crate::tree::file_identity(&source).unwrap();
    set_acl(&source, false);
    assert_eq!(crate::tree::file_identity(&source).unwrap(), identity);
    assert!(snapshot.require_matches(&source).is_err());
}

#[test]
fn replacement_acl_refuses_null_dacl_instead_of_changing_its_meaning() {
    let scratch = Scratch::new();
    let parent = scratch.directory();
    let source = parent
        .create_replacement_file(OsStr::new("original"))
        .unwrap();
    // SAFETY: The live fixture handle grants WRITE_DAC. A null DACL is set on
    // only this synthetic file, then rejected by the capture API.
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                source.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        },
        0
    );
    assert!(ReplacementMetadata::capture(&source).is_err());
}

#[test]
fn replacement_acl_cleanup_restores_private_access_through_the_held_handle() {
    let scratch = Scratch::new();
    let parent = scratch.directory();
    let original = parent
        .create_replacement_file(OsStr::new("original"))
        .unwrap();
    set_acl(&original, true);
    let reviewed = ReplacementMetadata::capture(&original).unwrap();
    let stage = parent.create_child(OsStr::new("stage")).unwrap();
    let payload = stage
        .create_replacement_file(OsStr::new("payload"))
        .unwrap();
    let private = ReplacementMetadata::capture(&payload).unwrap();
    reviewed.apply_to(&payload).unwrap();
    crate::restore_private_file_access(&payload).unwrap();
    let restored = ReplacementMetadata::capture(&payload).unwrap();
    assert_eq!(restored.acl, private.acl);
    assert_ne!(restored.control & SE_DACL_PROTECTED, 0);
    reviewed.require_matches(&original).unwrap();
    crate::tree::remove(payload).unwrap();
    drop(original);
    stage.remove().unwrap();
    assert!(!scratch.0.join("stage").exists());
}

#[test]
fn replacement_acl_inherited_original_is_stable_when_moved_into_private_backup() {
    let scratch = Scratch::new();
    let name = OsStr::new("ordinary");
    fs::write(scratch.0.join(name), b"ordinary original").unwrap();
    let parent = scratch.directory();
    let original = parent.child_for_reviewed_removal(name, false).unwrap();
    let reviewed = ReplacementMetadata::capture(&original).unwrap();
    let backup = parent
        .create_backup_directory(OsStr::new("backup"))
        .unwrap();
    backup.publish_file(&original, name).unwrap();
    let moved = ReplacementMetadata::capture(&original).unwrap();
    assert!(
        reviewed == moved,
        "Original DACL changed on relocation: control before={:#06x}, after={:#06x}, ACL equal={}",
        reviewed.control,
        moved.control,
        reviewed.acl == moved.acl,
    );
    reviewed.require_matches(&original).unwrap();
    let replacement = backup
        .create_replacement_file(OsStr::new("replacement"))
        .unwrap();
    let result = reviewed.apply_to(&replacement);
    let applied = ReplacementMetadata::capture(&replacement).unwrap();
    assert!(
        result.is_ok(),
        "Inherited ACL copy: {result:?}; control {:#06x}->{:#06x}; ACEs {:?}->{:?}",
        reviewed.control,
        applied.control,
        ace_shapes(&reviewed),
        ace_shapes(&applied),
    );
    reviewed.require_applied(&replacement).unwrap();
}

fn ace_shapes(metadata: &ReplacementMetadata) -> Vec<(u8, u8, u32)> {
    let acl = metadata.acl.as_ptr().cast::<ACL>();
    // SAFETY: Capture fully validated this live ACL and all its ACEs.
    unsafe {
        (0..u32::from((*acl).AceCount))
            .map(|index| {
                let mut ace = null_mut();
                check(GetAce(acl, index, &mut ace)).unwrap();
                let header = &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>();
                let mask = (*ace.cast::<ACCESS_ALLOWED_ACE>()).Mask;
                (header.AceType, header.AceFlags, mask)
            })
            .collect()
    }
}
