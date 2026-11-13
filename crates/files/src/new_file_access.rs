//! Effective file ACL inheritance from the reviewed destination directory.

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN};
use windows_sys::Win32::Security::{
    CreatePrivateObjectSecurityEx, DACL_SECURITY_INFORMATION, DestroyPrivateObjectSecurity,
    GENERIC_MAPPING, GROUP_SECURITY_INFORMATION, GetKernelObjectSecurity,
    GetSecurityDescriptorLength, IsValidSecurityDescriptor, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, SEF_DACL_AUTO_INHERIT, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

use crate::{ReplacementMetadata, tree::Directory};

const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
const PARENT_INFORMATION: u32 =
    OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;

/// Captures the ACL an ordinary file would inherit from its destination parent.
/// Windows performs ACE propagation and creator-principal substitution. The
/// Browser capture applies Java LocalFinalizer's explicit, unprotected ACL list;
/// ordinary capture retains native inherited ACE flags and DACL control.
#[derive(Clone, Eq, PartialEq)]
pub struct NewFileAccess {
    parent_identity: (u64, [u8; 16]),
    parent_access: ReplacementMetadata,
    inherited: ReplacementMetadata,
    ordinary: bool,
    directory: bool,
}

impl std::fmt::Debug for NewFileAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewFileAccess")
            .finish_non_exhaustive()
    }
}

impl NewFileAccess {
    pub fn capture(parent: &Directory) -> io::Result<Self> {
        Self::capture_with(parent, false, false)
    }

    /// Captures ordinary operating-system file creation, retaining inherited
    /// ACE flags and DACL control instead of Java's explicit ACL-list setter.
    pub fn capture_ordinary(parent: &Directory) -> io::Result<Self> {
        Self::capture_with(parent, true, false)
    }

    /// Captures ordinary directory creation, including propagation to descendants.
    pub fn capture_ordinary_directory(parent: &Directory) -> io::Result<Self> {
        Self::capture_with(parent, true, true)
    }

    fn capture_with(parent: &Directory, ordinary: bool, directory: bool) -> io::Result<Self> {
        let parent_descriptor = parent_descriptor(parent.file())?;
        // SAFETY: The bounded query returned a complete Windows descriptor.
        let parent_access = unsafe { ReplacementMetadata::from_descriptor(&parent_descriptor) }?;
        let parent_identity = parent.identity()?;
        let token = effective_token()?;
        let mapping = GENERIC_MAPPING {
            GenericRead: FILE_GENERIC_READ,
            GenericWrite: FILE_GENERIC_WRITE,
            GenericExecute: FILE_GENERIC_EXECUTE,
            GenericAll: FILE_ALL_ACCESS,
        };
        let mut derived = null_mut();
        // SAFETY: The parent security descriptor, token and file generic mapping
        // remain live. Windows allocates a descriptor owned by PrivateDescriptor.
        check(unsafe {
            CreatePrivateObjectSecurityEx(
                parent_descriptor.as_ptr().cast_mut().cast(),
                null_mut(),
                &mut derived,
                null(),
                i32::from(directory),
                SEF_DACL_AUTO_INHERIT,
                token.as_raw_handle(),
                &mapping,
            )
        })?;
        let derived = PrivateDescriptor(derived);
        // SAFETY: A successful inheritance call returned a live descriptor. Its
        // bounded self-relative bytes are copied into aligned owned storage.
        let inherited = unsafe {
            if derived.0.is_null() || IsValidSecurityDescriptor(derived.0) == 0 {
                return Err(invalid(
                    "Windows returned an invalid inherited file descriptor",
                ));
            }
            let length = GetSecurityDescriptorLength(derived.0) as usize;
            if !(20..=MAX_DESCRIPTOR_BYTES).contains(&length) {
                return Err(invalid(
                    "Inherited Windows file permissions exceed their limit",
                ));
            }
            let mut bytes = vec![0_u64; length.div_ceil(size_of::<u64>())];
            std::ptr::copy_nonoverlapping(
                derived.0.cast::<u8>(),
                bytes.as_mut_ptr().cast(),
                length,
            );
            ReplacementMetadata::from_descriptor(&bytes)?
        };
        let inherited = if ordinary {
            inherited.for_native_child(&parent_access)?
        } else {
            inherited
        };
        Ok(Self {
            parent_identity,
            parent_access,
            inherited,
            ordinary,
            directory,
        })
    }

    pub fn require_parent(&self, parent: &Directory) -> io::Result<()> {
        let descriptor = parent_descriptor(parent.file())?;
        // SAFETY: The bounded query returned a complete Windows descriptor.
        let current = unsafe { ReplacementMetadata::from_descriptor(&descriptor) }?;
        if parent.identity()? == self.parent_identity && current == self.parent_access {
            Ok(())
        } else {
            Err(invalid(
                "The destination directory permissions changed during the transfer",
            ))
        }
    }

    pub fn apply_to(&self, file: &File) -> io::Result<()> {
        if self.directory {
            return Err(invalid("Directory inheritance cannot be applied to a file"));
        }
        if self.ordinary {
            self.inherited.apply_native_to(file)
        } else {
            self.inherited.apply_to(file)
        }
    }

    pub fn require_applied(&self, file: &File) -> io::Result<()> {
        if self.directory {
            return Err(invalid("Directory inheritance cannot describe a file"));
        }
        if self.ordinary {
            self.inherited.require_matches(file)
        } else {
            self.inherited.require_applied(file)
        }
    }

    /// Applies native directory inheritance to an empty unpublished payload root.
    pub fn apply_to_directory(&self, directory: &Directory) -> io::Result<()> {
        if !self.directory || !self.ordinary {
            return Err(invalid("File inheritance cannot be applied to a directory"));
        }
        self.inherited.apply_native_to_directory(directory)
    }

    pub fn require_applied_directory(&self, directory: &Directory) -> io::Result<()> {
        if !self.directory || !self.ordinary {
            return Err(invalid("File inheritance cannot describe a directory"));
        }
        self.inherited.require_matches_directory(directory)
    }
}

struct PrivateDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for PrivateDescriptor {
    fn drop(&mut self) {
        // SAFETY: This guard owns the descriptor returned by the inheritance API.
        unsafe { DestroyPrivateObjectSecurity(std::ptr::from_mut(&mut self.0)) };
    }
}

fn effective_token() -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    // SAFETY: Current-thread/process pseudo-handles are valid. The output slot
    // receives one independently owned query handle on success.
    unsafe {
        if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
                return Err(error);
            }
            check(OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token,
            ))?;
        }
        Ok(OwnedHandle::from_raw_handle(token))
    }
}

fn parent_descriptor(file: &File) -> io::Result<Vec<u64>> {
    let mut length = 0;
    // SAFETY: A live directory handle grants READ_CONTROL and the count slot is writable.
    let measured = unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            PARENT_INFORMATION,
            null_mut(),
            0,
            &mut length,
        )
    };
    if measured != 0
        || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(invalid(
            "Could not measure the destination directory permissions",
        ));
    }
    let size = length as usize;
    if !(20..=MAX_DESCRIPTOR_BYTES).contains(&size) {
        return Err(invalid(
            "Destination directory permissions exceed their limit",
        ));
    }
    let mut bytes = vec![0_u64; size.div_ceil(size_of::<u64>())];
    // SAFETY: The aligned buffer holds the measured descriptor. A concurrent
    // increase fails the query instead of publishing an incomplete ACL.
    check(unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            PARENT_INFORMATION,
            bytes.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    })?;
    // SAFETY: The successful bounded query initialized this aligned descriptor.
    unsafe {
        let pointer = bytes.as_mut_ptr().cast();
        if IsValidSecurityDescriptor(pointer) == 0
            || GetSecurityDescriptorLength(pointer) as usize > size
        {
            return Err(invalid(
                "Windows returned invalid destination directory permissions",
            ));
        }
    }
    Ok(bytes)
}

fn check(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
