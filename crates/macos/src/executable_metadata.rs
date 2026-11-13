//! Descriptor-relative security metadata for an already opened executable.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;
use std::ptr::{NonNull, null_mut};

const MAX_NAMES: usize = 16 * 1024;
const MAX_ATTRIBUTES: usize = 128;
const MAX_ATTRIBUTE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ACL_TEXT: usize = 64 * 1024;
const ACL_TYPE_EXTENDED: c_uint = 0x100;
const COPYFILE_ACL: c_uint = 1;
const XATTR_CREATE: c_int = 2;
const PROVENANCE: &CStr = c"com.apple.provenance";

unsafe extern "C" {
    fn flistxattr(fd: c_int, names: *mut c_char, size: usize, flags: c_int) -> isize;
    fn fgetxattr(
        fd: c_int,
        name: *const c_char,
        value: *mut c_void,
        size: usize,
        position: u32,
        flags: c_int,
    ) -> isize;
    fn fsetxattr(
        fd: c_int,
        name: *const c_char,
        value: *const c_void,
        size: usize,
        position: u32,
        flags: c_int,
    ) -> c_int;
    fn fcopyfile(source: c_int, destination: c_int, state: *mut c_void, flags: c_uint) -> c_int;
    fn acl_get_fd_np(fd: c_int, kind: c_uint) -> *mut c_void;
    fn acl_to_text(acl: *mut c_void, length: *mut isize) -> *mut c_char;
    fn acl_free(value: *mut c_void) -> c_int;
}

#[derive(Eq, PartialEq)]
struct Metadata {
    attributes: Vec<(CString, Vec<u8>)>,
    acl: Option<Vec<u8>>,
}

/// The metadata expected on both opened files after staging.
pub struct ExecutableMetadata {
    source: Metadata,
    extra_staged_provenance: Option<Vec<u8>>,
}

impl ExecutableMetadata {
    fn new(source: Metadata, mut staged: Metadata) -> Self {
        // macOS can stamp the newly created file with its own provenance. Keep that
        // value only when the source has no provenance to preserve.
        let extra_staged_provenance = if source.provenance_index().is_none() {
            staged.take_provenance()
        } else {
            None
        };
        Self {
            source,
            extra_staged_provenance,
        }
    }

    /// Checks that the source and the staged copy still have their recorded metadata.
    pub fn verify(&self, source: &File, staged: &File) -> io::Result<()> {
        if Metadata::read(source)? != self.source {
            return Err(changed());
        }
        self.require_staged(Metadata::read(staged)?)
    }

    fn require_staged(&self, mut staged: Metadata) -> io::Result<()> {
        if let Some(expected) = &self.extra_staged_provenance
            && staged.take_provenance().as_ref() != Some(expected)
        {
            return Err(changed());
        }
        if staged != self.source {
            return Err(changed());
        }
        Ok(())
    }
}

/// Copies bounded descriptor attributes, including quarantine, and the source ACL to a fresh file.
/// Any omitted, altered or unsupported source attribute fails the copy.
pub fn copy_executable_metadata(
    source: &File,
    destination: &File,
) -> io::Result<ExecutableMetadata> {
    let metadata = ExecutableMetadata::new(Metadata::read(source)?, Metadata::read(destination)?);
    for (name, value) in &metadata.source.attributes {
        // SAFETY: The file descriptor, NUL-terminated name and value buffer stay live for the call.
        if unsafe {
            fsetxattr(
                destination.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                XATTR_CREATE,
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            // macOS can attach attributes before we copy them. Keep the existing value
            // and let the full comparison below reject any difference.
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
    if metadata.source.acl.is_some() {
        // SAFETY: Both descriptors are live. A null state requests the native default for ACL-only copy.
        if unsafe {
            fcopyfile(
                source.as_raw_fd(),
                destination.as_raw_fd(),
                null_mut(),
                COPYFILE_ACL,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    metadata.verify(source, destination)?;
    Ok(metadata)
}

impl Metadata {
    fn provenance_index(&self) -> Option<usize> {
        self.attributes
            .iter()
            .position(|(name, _)| name.as_c_str() == PROVENANCE)
    }

    fn take_provenance(&mut self) -> Option<Vec<u8>> {
        let index = self.provenance_index()?;
        Some(self.attributes.remove(index).1)
    }

    fn read(file: &File) -> io::Result<Self> {
        let fd = file.as_raw_fd();
        let mut names = vec![0_u8; MAX_NAMES];
        // SAFETY: The buffer holds MAX_NAMES writable bytes and the descriptor is live.
        let count =
            read_size(unsafe { flistxattr(fd, names.as_mut_ptr().cast(), names.len(), 0) })?;
        let names = names.get(..count).ok_or_else(limit)?;
        let mut attributes = Vec::new();
        let mut remaining = MAX_ATTRIBUTE_BYTES;
        for name in names.split_inclusive(|byte| *byte == 0) {
            if attributes.len() >= MAX_ATTRIBUTES {
                return Err(limit());
            }
            let name = CStr::from_bytes_with_nul(name).map_err(|_| changed())?;
            if name.is_empty() {
                return Err(changed());
            }
            // SAFETY: A null pointer and zero size request the size of this live attribute.
            let size = read_size(unsafe { fgetxattr(fd, name.as_ptr(), null_mut(), 0, 0, 0) })?;
            remaining = remaining.checked_sub(size).ok_or_else(limit)?;
            let mut value = vec![0_u8; size];
            // SAFETY: The writable buffer has the supplied size. The descriptor and name are live.
            let read = read_size(unsafe {
                fgetxattr(fd, name.as_ptr(), value.as_mut_ptr().cast(), size, 0, 0)
            })?;
            if read != size {
                return Err(changed());
            }
            attributes.push((name.to_owned(), value));
        }
        attributes.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Ok(Self {
            attributes,
            acl: acl_text(file)?,
        })
    }
}

struct AclMemory(NonNull<c_void>);

impl Drop for AclMemory {
    fn drop(&mut self) {
        // SAFETY: This guard owns a non-null allocation from acl_get_fd_np or acl_to_text.
        unsafe { acl_free(self.0.as_ptr()) };
    }
}

fn acl_text(file: &File) -> io::Result<Option<Vec<u8>>> {
    // SAFETY: The descriptor stays live for the native lookup.
    let Some(acl) = NonNull::new(unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) })
    else {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    };
    let acl = AclMemory(acl);
    let mut size = 0;
    // SAFETY: The ACL is owned and live. The length output is writable.
    let text = NonNull::new(unsafe { acl_to_text(acl.0.as_ptr(), &mut size) })
        .ok_or_else(io::Error::last_os_error)?;
    let text = AclMemory(text.cast());
    let size = read_size(size)?;
    if size > MAX_ACL_TEXT {
        return Err(limit());
    }
    // SAFETY: The native allocator returned a text buffer with the recorded byte length.
    Ok(Some(
        unsafe { std::slice::from_raw_parts(text.0.as_ptr().cast::<u8>(), size) }.to_vec(),
    ))
}

fn read_size(value: isize) -> io::Result<usize> {
    usize::try_from(value).map_err(|_| io::Error::last_os_error())
}

fn changed() -> io::Error {
    io::Error::other("The executable's security metadata changed during staging")
}

fn limit() -> io::Error {
    io::Error::other("The executable's security metadata exceeds the staging limit")
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cli_metadata_copies_quarantine_and_detects_a_later_change() -> io::Result<()> {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("kortty-cli-metadata-{}-{time}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let scratch = Scratch(path);
        let source = File::create_new(scratch.0.join("source"))?;
        let destination = File::create_new(scratch.0.join("destination"))?;
        let quarantine = c"com.apple.quarantine";
        let initial = b"0081;00000000;korTTY fixture;";
        let altered = b"0082;00000000;korTTY fixture;";
        let set = |value: &[u8]| {
            // SAFETY: The source and the complete NUL-terminated name and value are live.
            let result = unsafe {
                fsetxattr(
                    source.as_raw_fd(),
                    quarantine.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        };
        set(initial)?;
        let metadata = copy_executable_metadata(&source, &destination)?;
        metadata.verify(&source, &destination)?;
        copy_executable_metadata(&source, &destination)?;
        set(altered)?;
        assert!(metadata.verify(&source, &destination).is_err());
        assert!(copy_executable_metadata(&source, &destination).is_err());
        set(initial)?;
        metadata.verify(&source, &destination)?;
        Ok(())
    }

    #[test]
    fn cli_metadata_keeps_only_the_captured_destination_provenance() {
        let unmarked = || Metadata {
            attributes: Vec::new(),
            acl: None,
        };
        let marked = |value: &[u8]| Metadata {
            attributes: vec![(PROVENANCE.to_owned(), value.to_vec())],
            acl: None,
        };
        let metadata = ExecutableMetadata::new(unmarked(), marked(b"staged"));
        assert!(metadata.require_staged(marked(b"staged")).is_ok());
        assert!(metadata.require_staged(marked(b"changed")).is_err());
        assert!(metadata.require_staged(unmarked()).is_err());

        let mut extra = marked(b"staged");
        extra
            .attributes
            .push((c"com.apple.quarantine".to_owned(), b"unexpected".to_vec()));
        assert!(metadata.require_staged(extra).is_err());

        let metadata = ExecutableMetadata::new(unmarked(), unmarked());
        assert!(metadata.require_staged(marked(b"staged")).is_err());

        let metadata = ExecutableMetadata::new(marked(b"source"), marked(b"staged"));
        assert!(metadata.require_staged(marked(b"staged")).is_err());
        assert!(metadata.require_staged(marked(b"source")).is_ok());
    }
}
