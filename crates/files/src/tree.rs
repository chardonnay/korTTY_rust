//! Win32 directory-handle operations for confined file transfer.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
use std::path::{Component, Path, PathBuf};
use std::ptr::{from_ref, null, null_mut};
use std::time::Duration;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_INFORMATION_CLASS, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS,
    FILE_RENAME_REPLACE_IF_EXISTS, FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation,
    FileRenameInformationEx, NtCreateFile, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING, ERROR_DIR_NOT_EMPTY, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
    ERROR_REPARSE_POINT_ENCOUNTERED, ERROR_SHARING_VIOLATION, OBJ_CASE_INSENSITIVE,
    OBJ_DONT_REPARSE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Security::SECURITY_DESCRIPTOR;
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_ID_BOTH_DIR_INFO, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_TRAVERSE,
    FileDispositionInfo, FileDispositionInfoEx, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx,
    SECURITY_IDENTIFICATION, SYNCHRONIZE, SetFileInformationByHandle, WRITE_DAC,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

mod lease;
mod owners;
mod security;
pub use lease::{ExternalLease, SmbReadLease};
pub use owners::FileOwnerNames;
pub use security::restore_private_file_access;

const DIRECTORY_READ: u32 = FILE_GENERIC_READ | FILE_TRAVERSE;
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const DIRECTORY_PUBLISH_RETRY_DELAYS_MS: [u64; 5] = [10, 25, 50, 100, 200];

/// An open local directory. Child operations resolve one component from this handle.
pub struct Directory(File);

/// Returns whether Windows reports a reparse point on the inspected entry.
pub fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// The volume serial number and 128-bit identity of an open file or directory.
pub fn file_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the file and writable output record remain live.
    ok(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    })?;
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

/// The number of hard links to an open file.
pub fn file_link_count(file: &File) -> io::Result<u32> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: the file and writable output record remain live.
    ok(unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    })?;
    Ok(info.NumberOfLinks)
}

impl Directory {
    pub(crate) fn file(&self) -> &File {
        &self.0
    }

    /// Anchors a caller-resolved directory for same-directory staging and publication.
    pub fn open_publish_parent(path: &Path) -> io::Result<Self> {
        Self::from_file(
            OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(path)?,
        )
    }

    /// Opens each component from the volume root, refusing reparse points.
    pub fn open_path(path: &Path) -> io::Result<Self> {
        Self::open_path_with_share(path, SHARE_ALL)
    }

    /// Anchors a reviewed parent while preventing its removal or rename.
    pub fn open_reviewed_removal_parent(path: &Path) -> io::Result<Self> {
        Self::open_path_with_share(path, FILE_SHARE_READ | FILE_SHARE_WRITE)
    }

    fn open_path_with_share(path: &Path, share: u32) -> io::Result<Self> {
        let mut components = path.components();
        let (Some(Component::Prefix(prefix)), Some(Component::RootDir)) =
            (components.next(), components.next())
        else {
            return Err(invalid("Expected an absolute Windows directory"));
        };
        let mut volume = PathBuf::from(prefix.as_os_str());
        volume.push(r"\");
        let file = OpenOptions::new()
            .read(true)
            .access_mode(DIRECTORY_READ)
            .share_mode(share)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(volume)?;
        let mut directory = Self::from_file(file)?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid("Expected a canonical Windows directory"));
            };
            directory = Self::from_file(directory.open_with_share(
                name,
                DIRECTORY_READ,
                FILE_OPEN,
                FILE_DIRECTORY_FILE,
                None,
                share,
            )?)?;
        }
        Ok(directory)
    }

    /// Wraps an already opened, ordinary directory.
    pub fn from_file(file: File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(invalid("Directory is missing or is a reparse point"));
        }
        Ok(Self(file))
    }

    /// Opens a child for reading or returns `None` for a reparse point.
    pub fn read_child(&self, name: &OsStr) -> io::Result<Option<File>> {
        match self.open(name, FILE_GENERIC_READ, FILE_OPEN, 0, None) {
            Ok(file) => {
                let metadata = file.metadata()?;
                if is_reparse_point(&metadata) {
                    return Ok(None);
                }
                if metadata.is_dir() {
                    return self
                        .open(name, DIRECTORY_READ, FILE_OPEN, FILE_DIRECTORY_FILE, None)
                        .map(Some);
                }
                Ok(Some(file))
            }
            Err(error) if error.raw_os_error() == Some(ERROR_REPARSE_POINT_ENCOUNTERED as i32) => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Reads an ordinary child's attributes without requesting its contents.
    /// Reparse points and unsupported entry types are returned as `None`.
    pub fn open_entry_metadata(&self, name: &OsStr) -> io::Result<Option<File>> {
        let file = match self.open(name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, FILE_OPEN, 0, None) {
            Ok(file) => file,
            Err(error) if error.raw_os_error() == Some(ERROR_REPARSE_POINT_ENCOUNTERED as i32) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if is_reparse_point(&metadata) || !(metadata.is_file() || metadata.is_dir()) {
            return Ok(None);
        }
        Ok(Some(file))
    }

    /// Opens only a regular file's attributes, without requesting its contents.
    /// The returned handle can verify completed downloads whose final ACL denies reads.
    pub fn open_file_metadata(&self, name: &OsStr) -> io::Result<File> {
        let file = self
            .open_entry_metadata(name)?
            .ok_or_else(|| invalid("Expected regular file metadata without a reparse point"))?;
        if !file.metadata()?.is_file() {
            return Err(invalid(
                "Expected regular file metadata without a reparse point",
            ));
        }
        Ok(file)
    }

    /// Exclusively creates a private directory for the current Windows user.
    pub fn create_child(&self, name: &OsStr) -> io::Result<Self> {
        Self::from_file(self.create_private(name, DIRECTORY_READ, FILE_DIRECTORY_FILE)?)
    }

    /// Creates a private payload root whose held handle can install final directory access.
    pub fn create_tree_payload(&self, name: &OsStr) -> io::Result<Self> {
        Self::from_file(self.create_private_with_share(
            name,
            DIRECTORY_READ | WRITE_DAC,
            FILE_DIRECTORY_FILE,
            SHARE_ALL,
        )?)
    }

    /// Exclusively creates an ordinary transfer directory with its parent's ACL inheritance.
    pub fn create_transfer_child(&self, name: &OsStr) -> io::Result<Self> {
        Self::from_file(self.open(name, DIRECTORY_READ, FILE_CREATE, FILE_DIRECTORY_FILE, None)?)
    }

    /// Creates a private directory for relocating an opened original. Native
    /// rename may open the destination directory for write access internally.
    pub fn create_backup_directory(&self, name: &OsStr) -> io::Result<Self> {
        Self::from_file(self.create_private_with_share(
            name,
            DIRECTORY_READ,
            FILE_DIRECTORY_FILE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )?)
    }

    /// Exclusively creates a private regular file for the current Windows user.
    pub fn create_file(&self, name: &OsStr) -> io::Result<File> {
        self.create_private(
            name,
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES,
            FILE_NON_DIRECTORY_FILE,
        )
    }

    /// Exclusively creates a regular file with native parent inheritance inside
    /// an unpublished tree. The retained handle permits verified private cleanup.
    pub fn create_ordinary_file(&self, name: &OsStr) -> io::Result<File> {
        self.open(
            name,
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE | WRITE_DAC,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
            None,
        )
    }

    /// Creates a private replacement stage whose held handle can set its final DACL.
    pub fn create_replacement_file(&self, name: &OsStr) -> io::Result<File> {
        self.create_private(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC,
            FILE_NON_DIRECTORY_FILE,
        )
    }

    /// Creates a staging file with inherited permissions and delete access for publication.
    pub fn create_staging_file(&self, name: &OsStr) -> io::Result<File> {
        self.open(
            name,
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
            None,
        )
    }

    /// Creates a private read/write file with a handle suitable for an identity check.
    pub fn create_read_write_file(&self, name: &OsStr) -> io::Result<File> {
        self.create_private(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_NON_DIRECTORY_FILE,
        )
    }

    /// Creates a private download checkpoint whose held handle can apply final access.
    pub fn create_resumable_download_file(&self, name: &OsStr) -> io::Result<File> {
        self.create_private(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC,
            FILE_NON_DIRECTORY_FILE,
        )
    }

    /// Exclusively creates a private file and reopens it for cooperating byte-range lock probes.
    pub fn create_shared_lock_file(&self, name: &OsStr) -> io::Result<File> {
        let created = self.create_read_write_file(name)?;
        let identity = file_identity(&created)?;
        drop(created);
        let file = self.open_shared_lock_file(name)?;
        if file_identity(&file)? != identity {
            return Err(invalid("The lock file changed before it could be opened"));
        }
        Ok(file)
    }

    /// Opens an existing current-user lock with read, write and delete access.
    pub fn open_shared_lock_file(&self, name: &OsStr) -> io::Result<File> {
        let file = self.open(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            None,
        )?;
        Self::require_owner_only_file(&file)?;
        Ok(file)
    }

    /// Reopens a private download checkpoint with cooperating byte-range locks
    /// and permission updates through the same identity-checked handle.
    pub fn open_resumable_download_file(&self, name: &OsStr) -> io::Result<File> {
        let file = self.open(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE | WRITE_DAC,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
            None,
        )?;
        Self::require_owner_only_file(&file)?;
        Ok(file)
    }

    /// Checks for the same protected current-user ACL used by private directory creation.
    pub fn require_owner_only(&self) -> io::Result<()> {
        security::require_current_user_only(&self.0)
    }

    /// The identity of this open directory.
    pub fn identity(&self) -> io::Result<(u64, [u8; 16])> {
        file_identity(&self.0)
    }

    /// Reads metadata from the already opened directory handle.
    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.0.metadata()
    }

    /// Checks the type and protected current-user ACL of an opened file.
    pub fn require_owner_only_file(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse_point(&metadata) {
            return Err(invalid("Expected a regular file without a reparse point"));
        }
        security::require_current_user_only(file)
    }

    /// Checks an inherited current-user credential ACL, allowing system administrators.
    pub fn require_user_credential_file(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_reparse_point(&metadata) {
            return Err(invalid("Expected a regular file without a reparse point"));
        }
        security::require_user_and_administrators(file)
    }

    fn create_private(&self, name: &OsStr, access: u32, options: u32) -> io::Result<File> {
        self.create_private_with_share(name, access, options, FILE_SHARE_READ)
    }

    fn create_private_with_share(
        &self,
        name: &OsStr,
        access: u32,
        options: u32,
        share: u32,
    ) -> io::Result<File> {
        security::with_private_descriptor(|descriptor, sid| {
            let file = self.open_with_share(
                name,
                access | DELETE,
                FILE_CREATE,
                options,
                Some(descriptor),
                share,
            )?;
            if let Err(error) = security::require_owner_only(&file, sid) {
                let _ = remove(file);
                return Err(error);
            }
            Ok(file)
        })
    }

    fn open(
        &self,
        name: &OsStr,
        access: u32,
        disposition: u32,
        options: u32,
        security: Option<&SECURITY_DESCRIPTOR>,
    ) -> io::Result<File> {
        let share = if security.is_some() {
            FILE_SHARE_READ
        } else {
            SHARE_ALL
        };
        self.open_with_share(name, access, disposition, options, security, share)
    }

    fn open_with_share(
        &self,
        name: &OsStr,
        access: u32,
        disposition: u32,
        options: u32,
        security: Option<&SECURITY_DESCRIPTOR>,
        share: u32,
    ) -> io::Result<File> {
        let mut wide = component(name)?;
        let length = (wide.len() * size_of::<u16>()) as u16;
        let name = UNICODE_STRING {
            Length: length,
            MaximumLength: length,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.0.as_raw_handle(),
            ObjectName: &name,
            Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            SecurityDescriptor: security.map_or(null(), from_ref),
            ..OBJECT_ATTRIBUTES::default()
        };
        let mut handle = null_mut();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: the root is a live directory; all records and the counted UTF-16 name
        // remain live through this synchronous, one-component operation.
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                access,
                &attributes,
                &mut status,
                null(),
                FILE_ATTRIBUTE_NORMAL,
                share,
                disposition,
                options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                null(),
                0,
            )
        };
        nt_result(result)?;
        if handle.is_null() {
            return Err(io::Error::other("Windows returned no child handle"));
        }
        // SAFETY: a successful open returned one owned handle.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    /// Compares the volume and file IDs returned for two open directories.
    pub fn same_as(&self, other: &Self) -> io::Result<bool> {
        Ok(file_identity(&self.0)? == file_identity(&other.0)?)
    }

    /// Lists a bounded number of names from the open directory handle.
    pub fn names(&self, limit: usize) -> io::Result<Vec<OsString>> {
        let mut buffer = [0_u64; 1024];
        let mut class = FileIdBothDirectoryRestartInfo;
        let mut names = Vec::new();
        loop {
            // SAFETY: the directory and aligned writable buffer remain live.
            let result = unsafe {
                GetFileInformationByHandleEx(
                    self.0.as_raw_handle(),
                    class,
                    buffer.as_mut_ptr().cast(),
                    size_of_val(&buffer) as u32,
                )
            };
            if let Err(error) = ok(result) {
                if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    return Ok(names);
                }
                return Err(error);
            }
            append_names(&buffer, &mut names, limit)?;
            class = FileIdBothDirectoryInfo;
        }
    }

    /// Renames an opened ordinary directory into this directory without replacement.
    /// The source handle must have delete access.
    pub fn publish(&self, staged: &Self, destination: &OsStr) -> io::Result<()> {
        // Windows can deny directory renames while a descendant is temporarily open.
        // Keep both directory identities open and refuse replacement on every attempt.
        for delay_ms in DIRECTORY_PUBLISH_RETRY_DELAYS_MS {
            match self.publish_directory_once(staged, destination) {
                Err(error)
                    if error.kind() == io::ErrorKind::PermissionDenied
                        || error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) =>
                {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
                result => return result,
            }
        }
        self.publish_directory_once(staged, destination)
    }

    /// Attempts one handle-relative directory rename with replacement disabled.
    pub fn publish_directory_once(&self, staged: &Self, destination: &OsStr) -> io::Result<()> {
        self.publish_file(&staged.0, destination)
    }

    /// Returns the delay before retrying an access or sharing error from a
    /// directory publication. Guarded callers must recheck their authority.
    pub fn publish_directory_retry_delay(
        error: &io::Error,
        failed_attempt: usize,
    ) -> Option<Duration> {
        if matches!(
            error.raw_os_error().map(|code| code as u32),
            Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
        ) {
            DIRECTORY_PUBLISH_RETRY_DELAYS_MS
                .get(failed_attempt)
                .copied()
                .map(Duration::from_millis)
        } else {
            None
        }
    }

    /// Renames an opened staging file into this directory without replacement.
    /// The file handle must have delete access and stay open through publication.
    pub fn publish_file(&self, staged: &File, destination: &OsStr) -> io::Result<()> {
        self.rename_file(staged, destination, FileRenameInformation, 0)
    }

    /// Atomically publishes an opened staging file, replacing the destination if present.
    pub fn replace_file(&self, staged: &File, destination: &OsStr) -> io::Result<()> {
        self.rename_file(
            staged,
            destination,
            FileRenameInformation,
            FILE_RENAME_REPLACE_IF_EXISTS,
        )
    }

    /// Replaces a file from its open staging handle, including a destination shared for delete.
    pub fn replace_file_with_shared_readers(
        &self,
        staged: &File,
        destination: &OsStr,
    ) -> io::Result<()> {
        match self.replace_file(staged, destination) {
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                match self.rename_file(
                    staged,
                    destination,
                    FileRenameInformationEx,
                    FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS,
                ) {
                    Err(unsupported)
                        if unsupported.raw_os_error().is_some_and(|code| {
                            matches!(
                                code as u32,
                                ERROR_INVALID_FUNCTION
                                    | ERROR_INVALID_PARAMETER
                                    | ERROR_NOT_SUPPORTED
                            )
                        }) =>
                    {
                        Err(error)
                    }
                    result => result,
                }
            }
            result => result,
        }
    }

    fn rename_file(
        &self,
        staged: &File,
        destination: &OsStr,
        class: FILE_INFORMATION_CLASS,
        flags: u32,
    ) -> io::Result<()> {
        let wide = component(destination)?;
        const HEADER: usize = offset_of!(FILE_RENAME_INFORMATION, FileName);
        let mut buffer = [0_u64; 72];
        let bytes = wide.len() * size_of::<u16>();
        let size = size_of::<FILE_RENAME_INFORMATION>() + bytes;
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: the aligned backing buffer fits the initialized fixed header and
        // a component (at most 255 UTF-16 units). Both handles and status remain live.
        nt_result(unsafe {
            if class == FileRenameInformationEx {
                (*info).Anonymous.Flags = flags;
            } else {
                (*info).Anonymous.ReplaceIfExists = flags & FILE_RENAME_REPLACE_IF_EXISTS != 0;
            }
            (*info).RootDirectory = self.0.as_raw_handle();
            (*info).FileNameLength = bytes as u32;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                buffer.as_mut_ptr().cast::<u8>().add(HEADER).cast(),
                wide.len(),
            );
            NtSetInformationFile(
                staged.as_raw_handle(),
                &mut status,
                info.cast(),
                size as u32,
                class,
            )
        })
    }

    /// Opens a child for handle-based removal without resolving reparse points.
    pub fn child_for_removal(&self, name: &OsStr) -> io::Result<File> {
        self.open(name, DIRECTORY_READ | DELETE, FILE_OPEN, 0, None)
    }

    /// Opens an entry inside an owned private stage, allowing a short wait for
    /// a scanner that opened it without sharing delete access.
    pub fn child_for_stage_cleanup(&self, name: &OsStr) -> io::Result<File> {
        retry_stage_cleanup(|| self.child_for_removal(name), false)
    }

    /// Holds the exact ordinary child without sharing writes or deletion.
    pub fn child_for_reviewed_removal(&self, name: &OsStr, directory: bool) -> io::Result<File> {
        let (access, options) = if directory {
            (DIRECTORY_READ, FILE_DIRECTORY_FILE)
        } else {
            (FILE_GENERIC_READ, FILE_NON_DIRECTORY_FILE)
        };
        let file = self.open_with_share(
            name,
            access | DELETE,
            FILE_OPEN,
            options,
            None,
            FILE_SHARE_READ,
        )?;
        let metadata = file.metadata()?;
        if is_reparse_point(&metadata)
            || if directory {
                !metadata.is_dir()
            } else {
                !metadata.is_file()
            }
        {
            return Err(invalid("The reviewed local entry changed"));
        }
        Ok(file)
    }

    /// Checks the name after the disposition handle closed, without following links.
    pub fn require_absent_after_removal(&self, name: &OsStr) -> io::Result<()> {
        match self.open(name, FILE_GENERIC_READ, FILE_OPEN, 0, None) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) if error.raw_os_error() == Some(ERROR_DELETE_PENDING as i32) => Err(
                io::Error::other("Windows accepted the removal and is waiting for open handles"),
            ),
            Ok(_) => Err(io::Error::other(
                "The original local name is still occupied",
            )),
            Err(error) => Err(error),
        }
    }

    /// Removes this empty, staged directory when its handle closes.
    pub fn remove(self) -> io::Result<()> {
        remove(self.0)
    }

    /// Removes the name of an opened, empty private staging directory.
    pub fn discard_stage(self) -> io::Result<()> {
        discard_stage_entry(self.0)
    }
}

/// Removes an opened private stage entry from the directory namespace when this
/// handle closes, even while another compatible reader still holds the file.
pub fn discard_stage_entry(file: File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    let result = retry_stage_cleanup(
        || {
            // SAFETY: file and initialized disposition record remain live through this call.
            ok(unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileDispositionInfoEx,
                    (&info as *const FILE_DISPOSITION_INFO_EX).cast(),
                    size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
                )
            })
        },
        true,
    );
    match result {
        Err(error)
            if matches!(
                error.raw_os_error().map(|code| code as u32),
                Some(ERROR_INVALID_FUNCTION | ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED)
            ) =>
        {
            retry_stage_cleanup(|| mark_for_removal(&file), true)
        }
        result => result,
    }
}

fn retry_stage_cleanup<T>(
    mut operation: impl FnMut() -> io::Result<T>,
    held: bool,
) -> io::Result<T> {
    for delay_ms in DIRECTORY_PUBLISH_RETRY_DELAYS_MS {
        match operation() {
            Err(error)
                if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
                    || held
                        && matches!(
                            error.raw_os_error().map(|code| code as u32),
                            Some(ERROR_ACCESS_DENIED | ERROR_DIR_NOT_EMPTY)
                        ) =>
            {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            result => return result,
        }
    }
    operation()
}

/// Removes the opened child when its handle closes. Requires delete access.
pub fn remove(file: File) -> io::Result<()> {
    mark_for_removal(&file)
}

fn mark_for_removal(file: &File) -> io::Result<()> {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: file and initialized disposition record remain live through this call.
    ok(unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&info as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    })
}

fn append_names(buffer: &[u64], names: &mut Vec<OsString>, limit: usize) -> io::Result<()> {
    const HEADER: usize = offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let mut offset = 0;
    let length = size_of_val(buffer);
    loop {
        if length.saturating_sub(offset) < size_of::<FILE_ID_BOTH_DIR_INFO>() {
            return Err(invalid("Invalid Windows directory record"));
        }
        // SAFETY: the initialized buffer has enough bytes for the fixed record.
        let record = unsafe {
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<FILE_ID_BOTH_DIR_INFO>()
                .read_unaligned()
        };
        let next = record.NextEntryOffset as usize;
        let bytes = record.FileNameLength as usize;
        let end = HEADER + bytes;
        if bytes == 0
            || bytes > 510
            || !bytes.is_multiple_of(2)
            || end > length - offset
            || next != 0 && (next < end || next > length - offset || !next.is_multiple_of(8))
        {
            return Err(invalid("Invalid Windows directory name record"));
        }
        // SAFETY: offset and HEADER are UTF-16 aligned, and end was bounded above.
        let name = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>().add(offset + HEADER).cast(),
                bytes / 2,
            )
        };
        if !matches!(name, [46] | [46, 46]) {
            if names.len() == limit {
                return Err(io::Error::other("SFTP folder entry limit exceeded"));
            }
            names.push(OsString::from_wide(name));
        }
        if next == 0 {
            return Ok(());
        }
        offset += next;
    }
}

fn component(name: &OsStr) -> io::Result<Vec<u16>> {
    let wide: Vec<_> = name.encode_wide().take(256).collect();
    if wide.is_empty()
        || wide.len() > 255
        || matches!(wide.as_slice(), [46] | [46, 46])
        || wide.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92))
    {
        return Err(invalid("Expected one Windows file-name component"));
    }
    Ok(wide)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn ok(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn nt_result(status: i32) -> io::Result<()> {
    if status < 0 {
        // SAFETY: this converts an NTSTATUS value returned by an NT API.
        Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    } else {
        Ok(())
    }
}
