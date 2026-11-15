//! Checked Windows object metadata for an existing local tree replacement.

use windows_sys::Wdk::Storage::FileSystem::NtQueryEaFile;
use windows_sys::Win32::Foundation::{
    ERROR_HANDLE_EOF, STATUS_EAS_NOT_SUPPORTED, STATUS_NO_EAS_ON_FILE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN,
    FILE_ATTRIBUTE_NOT_CONTENT_INDEXED, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM,
    FILE_BASIC_INFO, FILE_STREAM_INFO, FILE_WRITE_ATTRIBUTES, FileBasicInfo, FileStreamInfo,
    GetFinalPathNameByHandleW,
};

use crate::ReplacementMetadata;

use super::*;

/// Both the stable file identity and the revision of a checked Windows entry.
pub struct ReviewedTreeMetadata {
    pub identity: (u64, [u8; 16]),
    pub access: ReplacementMetadata,
    pub size: u64,
    pub attributes: u32,
    pub created: i64,
    pub modified: i64,
    pub changed: i64,
}

const SUPPORTED_ATTRIBUTES: u32 = FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_DIRECTORY
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_NORMAL
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
    | FILE_ATTRIBUTE_READONLY
    | FILE_ATTRIBUTE_SYSTEM;

impl Directory {
    /// Creates a private directory with the rights needed for checked metadata and rename.
    pub fn create_reviewed_tree_stage(&self, name: &OsStr) -> io::Result<Self> {
        Self::from_file(self.create_private_with_share(
            name,
            DIRECTORY_READ | WRITE_DAC | FILE_WRITE_ATTRIBUTES,
            FILE_DIRECTORY_FILE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )?)
    }

    /// Refuses a short-name or alternate spelling for the held destination component.
    pub fn require_reviewed_tree_basename(&self, expected: &OsStr) -> io::Result<()> {
        require_reviewed_file_basename(&self.0, expected)
    }

    /// Captures the held directory's access, native identity and revision.
    pub fn reviewed_tree_metadata(&self) -> io::Result<ReviewedTreeMetadata> {
        reviewed_metadata(&self.0, true, || {
            ReplacementMetadata::capture_directory(self)
        })
    }

    /// Preserves the reviewed directory DACL, creation and modification times, and attributes.
    pub fn copy_reviewed_tree_metadata(&self, from: &Self) -> io::Result<()> {
        let expected = from.reviewed_tree_metadata()?;
        expected.access.apply_native_to_directory(self)?;
        apply_basic(&self.0, &expected)?;
        verify_copy(
            &expected,
            self.reviewed_tree_metadata()?,
            from.reviewed_tree_metadata()?,
        )
    }
}

/// Refuses a short-name or alternate spelling for a held Windows destination component.
pub fn require_reviewed_file_basename(file: &File, expected: &OsStr) -> io::Result<()> {
    let mut path = vec![0_u16; 32_768];
    // SAFETY: the directory and the UTF-16 output buffer remain live for this query.
    let count = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            0,
        )
    } as usize;
    if count == 0 {
        return Err(io::Error::last_os_error());
    }
    if count >= path.len() {
        return Err(changed());
    }
    let path = PathBuf::from(OsString::from_wide(&path[..count]));
    if !path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    {
        return Err(changed());
    }
    Ok(())
}

/// Captures a single-link regular file that the current account controls.
pub fn reviewed_tree_file_metadata(file: &File) -> io::Result<ReviewedTreeMetadata> {
    reviewed_metadata(file, false, || ReplacementMetadata::capture(file))
}

/// Copies the supported Windows metadata of a reviewed regular file.
pub fn copy_reviewed_tree_file_metadata(from: &File, to: &File) -> io::Result<()> {
    let expected = reviewed_tree_file_metadata(from)?;
    expected.access.apply_native_to(to)?;
    apply_basic(to, &expected)?;
    verify_copy(
        &expected,
        reviewed_tree_file_metadata(to)?,
        reviewed_tree_file_metadata(from)?,
    )
}

fn verify_copy(
    expected: &ReviewedTreeMetadata,
    actual: ReviewedTreeMetadata,
    original: ReviewedTreeMetadata,
) -> io::Result<()> {
    if actual.access != expected.access
        || actual.attributes != expected.attributes
        || actual.created != expected.created
        || actual.modified != expected.modified
        || original.identity != expected.identity
        || original.changed != expected.changed
        || original.access != expected.access
    {
        return Err(changed());
    }
    Ok(())
}

fn reviewed_metadata(
    file: &File,
    directory: bool,
    capture_access: impl FnOnce() -> io::Result<ReplacementMetadata>,
) -> io::Result<ReviewedTreeMetadata> {
    let before = basic(file)?;
    let metadata = file.metadata()?;
    if before.FileAttributes & !SUPPORTED_ATTRIBUTES != 0
        || is_reparse_point(&metadata)
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || file_link_count(file)? != 1
        }
    {
        return Err(changed());
    }
    security::require_reviewed_tree_control(file)?;
    require_no_named_streams(file, directory)?;
    require_no_extended_data(file)?;
    let identity = file_identity(file)?;
    let access = capture_access()?;
    let after = basic(file)?;
    if before.ChangeTime != after.ChangeTime {
        return Err(changed());
    }
    Ok(ReviewedTreeMetadata {
        identity,
        access,
        size: metadata.len(),
        attributes: before.FileAttributes,
        created: before.CreationTime,
        modified: before.LastWriteTime,
        changed: before.ChangeTime,
    })
}

fn basic(file: &File) -> io::Result<FILE_BASIC_INFO> {
    let mut info = FILE_BASIC_INFO::default();
    // SAFETY: the file and initialized writable result remain live for this query.
    ok(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw mut info).cast(),
            size_of_val(&info) as u32,
        )
    })?;
    Ok(info)
}

fn apply_basic(file: &File, expected: &ReviewedTreeMetadata) -> io::Result<()> {
    let info = FILE_BASIC_INFO {
        CreationTime: expected.created,
        LastWriteTime: expected.modified,
        FileAttributes: expected.attributes,
        ..FILE_BASIC_INFO::default()
    };
    // SAFETY: the checked stage has attribute-write access and the buffer remains live.
    ok(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            (&raw const info).cast(),
            size_of_val(&info) as u32,
        )
    })
}

fn require_no_named_streams(file: &File, directory: bool) -> io::Result<()> {
    let mut buffer = [0_u64; 128];
    // SAFETY: the aligned output buffer is large enough for the standard stream record
    // and the checked file handle remains live for the synchronous read.
    let result = ok(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStreamInfo,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
        )
    });
    if let Err(error) = result {
        return if directory && error.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
            Ok(())
        } else {
            Err(error)
        };
    }
    const STANDARD: [u16; 7] = [58, 58, 36, 68, 65, 84, 65]; // ::$DATA
    let stream = buffer.as_ptr().cast::<FILE_STREAM_INFO>();
    // SAFETY: Win32 returned a record in the live, aligned output buffer. The fixed
    // string read below fits this buffer and occurs only after checking the length.
    if unsafe {
        (*stream).NextEntryOffset == 0
            && (*stream).StreamNameLength as usize == size_of_val(&STANDARD)
            && *std::ptr::addr_of!((*stream).StreamName).cast::<[u16; 7]>() == STANDARD
    } {
        Ok(())
    } else {
        Err(changed())
    }
}

fn require_no_extended_data(file: &File) -> io::Result<()> {
    let mut status = IO_STATUS_BLOCK::default();
    let mut buffer = [0_u64; 32];
    // SAFETY: only one held file is queried; the writable output and status remain live.
    let status = unsafe {
        NtQueryEaFile(
            file.as_raw_handle(),
            &mut status,
            buffer.as_mut_ptr().cast(),
            size_of_val(&buffer) as u32,
            true,
            null(),
            0,
            null(),
            true,
        )
    };
    if matches!(status, STATUS_NO_EAS_ON_FILE | STATUS_EAS_NOT_SUPPORTED) {
        Ok(())
    } else {
        Err(changed())
    }
}

fn changed() -> io::Error {
    invalid(
        "The reviewed Windows tree has unsupported access, types, attributes or streams, or changed while checking",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn private_windows_stage_discards_readonly_entries_by_handle() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let workspace = Directory::open_path(&root).unwrap();
        let stage = workspace
            .create_backup_directory(OsStr::new("stage"))
            .unwrap();
        let child_name = OsStr::new("empty");
        let file_name = OsStr::new("data");
        drop(stage.create_child(child_name).unwrap());
        let file = stage.create_file(file_name).unwrap();
        (&file).write_all(b"read-only source").unwrap();
        drop(file);
        let stage_path = root.join("stage");
        let sibling = root.join("outside-stage");
        std::fs::hard_link(stage_path.join(file_name), &sibling).unwrap();
        for name in [child_name, file_name] {
            let path = stage_path.join(name);
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        super::super::discard_stage_entry(stage.child_for_stage_cleanup(file_name).unwrap())
            .unwrap();
        Directory::from_file(stage.child_for_stage_cleanup(child_name).unwrap())
            .unwrap()
            .discard_stage()
            .unwrap();
        stage.discard_stage().unwrap();

        assert!(!stage_path.exists());
        assert_eq!(std::fs::read(&sibling).unwrap(), b"read-only source");
        assert!(
            std::fs::metadata(&sibling)
                .unwrap()
                .permissions()
                .readonly()
        );
        super::super::discard_stage_entry(
            workspace
                .child_for_stage_cleanup(OsStr::new("outside-stage"))
                .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn reviewed_windows_tree_copies_metadata_and_renames_only_to_a_vacant_name() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let old_name = OsStr::new("old");
        let private_name = OsStr::new("stage");
        let file_name = OsStr::new("file");
        let old = parent.create_reviewed_tree_stage(old_name).unwrap();
        let old_file = old.create_replacement_file(file_name).unwrap();
        (&old_file).write_all(b"test file").unwrap();
        old_file.sync_all().unwrap();
        let stage = parent.create_reviewed_tree_stage(private_name).unwrap();
        let candidate = stage
            .create_reviewed_tree_stage(OsStr::new("candidate"))
            .unwrap();
        let staged_file = candidate.create_replacement_file(file_name).unwrap();
        (&staged_file).write_all(b"new bytes").unwrap();
        staged_file.sync_all().unwrap();
        copy_reviewed_tree_file_metadata(&old_file, &staged_file).unwrap();
        candidate.copy_reviewed_tree_metadata(&old).unwrap();
        assert_eq!(
            reviewed_tree_file_metadata(&staged_file).unwrap().access,
            reviewed_tree_file_metadata(&old_file).unwrap().access,
        );
        drop((old_file, staged_file, old));

        let held = Directory::from_file(parent.child_for_reviewed_removal(old_name, true).unwrap())
            .unwrap();
        held.require_reviewed_tree_basename(old_name).unwrap();
        assert!(parent.child_for_reviewed_removal(old_name, true).is_err());
        let stamp = held.reviewed_tree_metadata().unwrap();
        assert!(parent.publish_directory_once(&candidate, old_name).is_err());
        let backup_name = OsStr::new("original");
        stage.publish_directory_once(&held, backup_name).unwrap();
        let backup = Directory::from_file(stage.read_child(backup_name).unwrap().unwrap()).unwrap();
        assert_eq!(backup.identity().unwrap(), stamp.identity);
        drop(backup);
        parent.publish_directory_once(&held, old_name).unwrap();
        let restored = Directory::from_file(parent.read_child(old_name).unwrap().unwrap()).unwrap();
        assert_eq!(restored.identity().unwrap(), stamp.identity);
        assert_eq!(std::fs::read(root.join("old/file")).unwrap(), b"test file");
    }

    #[test]
    fn reviewed_windows_tree_rejects_named_streams_and_multilink_regular_files() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let file_name = OsStr::new("file");
        let file = parent.create_replacement_file(file_name).unwrap();
        assert!(reviewed_tree_file_metadata(&file).is_ok());
        drop(file);
        std::fs::hard_link(root.join(file_name), root.join("alias")).unwrap();
        let file = parent.read_child(file_name).unwrap().unwrap();
        assert!(reviewed_tree_file_metadata(&file).is_err());
        drop(file);
        std::fs::remove_file(root.join("alias")).unwrap();
        std::fs::write(root.join("file:opaque"), b"alternate stream").unwrap();
        let file = parent.read_child(file_name).unwrap().unwrap();
        assert!(reviewed_tree_file_metadata(&file).is_err());
    }

    #[test]
    fn reviewed_windows_tree_handles_inherited_access_and_keeps_revision_when_moved() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch_root = scratch.path().canonicalize().unwrap();
        let workspace = Directory::open_path(&scratch_root).unwrap();
        drop(
            workspace
                .create_reviewed_tree_stage(OsStr::new("owned"))
                .unwrap(),
        );
        let root = scratch_root.join("owned");
        let receiver = OsStr::new("receiver");
        let name = OsStr::new("data");
        std::fs::create_dir(root.join(receiver)).unwrap();
        std::fs::write(root.join(receiver).join(name), b"original").unwrap();
        let parent = Directory::open_reviewed_removal_parent(&root).unwrap();
        let parent_before = parent.reviewed_tree_metadata().unwrap();
        let original =
            Directory::from_file(parent.child_for_reviewed_removal(receiver, true).unwrap())
                .unwrap();
        let original_before = original.reviewed_tree_metadata().unwrap();
        let original_file = original.read_child(name).unwrap().unwrap();
        let file_before = reviewed_tree_file_metadata(&original_file).unwrap();
        drop(original_file);

        let stage = parent
            .create_reviewed_tree_stage(OsStr::new("private"))
            .unwrap();
        let checked = stage.create_transfer_child(OsStr::new("verified")).unwrap();
        let checked_nested = checked.create_transfer_child(OsStr::new("nested")).unwrap();
        let checked_file = checked_nested.create_ordinary_file(name).unwrap();
        checked.reviewed_tree_metadata().unwrap();
        checked_nested.reviewed_tree_metadata().unwrap();
        drop(checked_file);
        let checked_file = checked_nested.read_child(name).unwrap().unwrap();
        reviewed_tree_file_metadata(&checked_file).unwrap();
        drop((checked_file, checked_nested, checked));

        let result = stage
            .create_reviewed_tree_stage(OsStr::new("candidate"))
            .unwrap();
        let result_file = result.create_replacement_file(name).unwrap();
        (&result_file).write_all(b"next").unwrap();
        let original_file = original.read_child(name).unwrap().unwrap();
        copy_reviewed_tree_file_metadata(&original_file, &result_file).unwrap();
        result.copy_reviewed_tree_metadata(&original).unwrap();
        let result_before = result.reviewed_tree_metadata().unwrap();
        let result_file_before = reviewed_tree_file_metadata(&result_file).unwrap();
        drop((original_file, result_file));

        stage
            .publish_directory_once(&original, OsStr::new("original"))
            .unwrap();
        parent.publish_directory_once(&result, receiver).unwrap();
        let parent_after = parent.reviewed_tree_metadata().unwrap();
        assert_eq!(parent_before.identity, parent_after.identity);
        assert_eq!(parent_before.access, parent_after.access);
        assert_eq!(parent_before.attributes, parent_after.attributes);
        require_same_moved_metadata(
            &original_before,
            &original.reviewed_tree_metadata().unwrap(),
        );
        require_same_moved_metadata(&result_before, &result.reviewed_tree_metadata().unwrap());
        let original_file = original.read_child(name).unwrap().unwrap();
        let result_file = result.read_child(name).unwrap().unwrap();
        let after_original_file = reviewed_tree_file_metadata(&original_file).unwrap();
        let after_result_file = reviewed_tree_file_metadata(&result_file).unwrap();
        require_same_moved_metadata(&file_before, &after_original_file);
        require_same_moved_metadata(&result_file_before, &after_result_file);
        assert_eq!(file_before.changed, after_original_file.changed);
        assert_eq!(result_file_before.changed, after_result_file.changed);
        assert_eq!(
            std::fs::read(root.join(receiver).join(name)).unwrap(),
            b"next"
        );
        assert_eq!(
            std::fs::read(root.join("private/original").join(name)).unwrap(),
            b"original"
        );
    }

    fn require_same_moved_metadata(before: &ReviewedTreeMetadata, after: &ReviewedTreeMetadata) {
        assert_eq!(before.identity, after.identity);
        assert_eq!(before.access, after.access);
        assert_eq!(before.size, after.size);
        assert_eq!(before.attributes, after.attributes);
        assert_eq!(before.modified, after.modified);
        assert_eq!(before.created, after.created);
    }
}
