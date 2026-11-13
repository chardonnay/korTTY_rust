//! Reviewed Windows DACL entries for a staged replacement file.

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::io::AsRawHandle as _;
use std::ptr::{from_mut, null_mut};

use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, GetAce,
    GetAclInformation, GetKernelObjectSecurity, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorLength, INHERITED_ACE,
    InitializeSecurityDescriptor, IsValidAcl, IsValidSecurityDescriptor, SE_DACL_AUTO_INHERIT_REQ,
    SE_DACL_AUTO_INHERITED, SE_DACL_DEFAULTED, SE_DACL_PROTECTED, SE_SELF_RELATIVE,
    SECURITY_DESCRIPTOR, SetKernelObjectSecurity, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl,
};
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;

const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024;
const REVIEWED_CONTROL: u16 =
    SE_DACL_DEFAULTED | SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ;

/// Binds the original DACL entry sequence and its inheritance control. The
/// applied ACL follows Java's explicit, unprotected final-part semantics.
#[derive(Clone, Eq, PartialEq)]
pub struct ReplacementMetadata {
    acl: Vec<u32>,
    control: u16,
}

impl std::fmt::Debug for ReplacementMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplacementMetadata")
            .finish_non_exhaustive()
    }
}

impl ReplacementMetadata {
    /// New filesystem objects retain their parent's legacy or automatic ACL
    /// inheritance model. Defaulted and request flags are input-only controls.
    pub(crate) fn for_native_child(&self, parent: &Self) -> io::Result<Self> {
        if parent.control & SE_DACL_AUTO_INHERITED != 0 {
            Ok(Self {
                acl: self.acl.clone(),
                control: SE_DACL_AUTO_INHERITED,
            })
        } else {
            Ok(Self {
                acl: self.final_acl()?,
                control: 0,
            })
        }
    }
    pub fn capture(file: &File) -> io::Result<Self> {
        Self::capture_object(file, false)
    }

    /// Encodes the ordered ACL entries and inheritance flags for a review proof.
    pub fn review_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(size_of::<u16>() + size_of_val(self.acl.as_slice()));
        bytes.extend(self.control.to_be_bytes());
        bytes.extend(self.acl.iter().flat_map(|word| word.to_le_bytes()));
        bytes
    }

    /// Captures the exact ordered DACL and inheritance control of an ordinary directory.
    pub fn capture_directory(directory: &crate::tree::Directory) -> io::Result<Self> {
        Self::capture_object(directory.file(), true)
    }

    fn capture_object(file: &File, directory: bool) -> io::Result<Self> {
        require_object(file, directory)?;
        let mut needed = 0;
        // SAFETY: This length query uses a live file handle and a writable count.
        let result = unsafe {
            GetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                null_mut(),
                0,
                &mut needed,
            )
        };
        if result != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(io::Error::other(
                "Could not measure the reviewed Windows DACL",
            ));
        }
        let bytes = usize::try_from(needed).map_err(io::Error::other)?;
        if !(20..=MAX_DESCRIPTOR_BYTES).contains(&bytes) {
            return Err(invalid(
                "The reviewed Windows security descriptor exceeds its limit",
            ));
        }
        let mut descriptor = vec![0_u64; bytes.div_ceil(size_of::<u64>())];
        let pointer = descriptor.as_mut_ptr().cast();
        // SAFETY: The aligned buffer has at least the queried size and stays live
        // while the validated descriptor's interior ACL pointer is inspected.
        unsafe {
            check(GetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                pointer,
                needed,
                &mut needed,
            ))?;
        }
        // SAFETY: GetKernelObjectSecurity returned this bounded Windows descriptor.
        unsafe { Self::from_descriptor(&descriptor) }
    }

    /// # Safety
    /// The slice must contain a Windows-issued self-relative security descriptor
    /// whose full variable-length components lie within the slice.
    pub(crate) unsafe fn from_descriptor(descriptor: &[u64]) -> io::Result<Self> {
        let bytes = size_of_val(descriptor);
        if !(20..=MAX_DESCRIPTOR_BYTES).contains(&bytes) {
            return Err(invalid("The Windows security descriptor exceeds its limit"));
        }
        let pointer = descriptor.as_ptr().cast_mut().cast();
        // SAFETY: The aligned descriptor came from a bounded Windows security
        // query or inheritance operation and remains live throughout inspection.
        unsafe {
            if IsValidSecurityDescriptor(pointer) == 0
                || GetSecurityDescriptorLength(pointer) as usize > bytes
            {
                return Err(invalid(
                    "Windows returned an invalid replacement security descriptor",
                ));
            }
            let (mut control, mut revision) = (0, 0);
            check(GetSecurityDescriptorControl(
                pointer,
                &mut control,
                &mut revision,
            ))?;
            if control & SE_SELF_RELATIVE == 0 {
                return Err(invalid(
                    "The replacement security descriptor is not self-relative",
                ));
            }
            let (mut present, mut defaulted) = (0, 0);
            let mut acl = null_mut();
            check(GetSecurityDescriptorDacl(
                pointer,
                &mut present,
                &mut acl,
                &mut defaulted,
            ))?;
            if present == 0 || acl.is_null() {
                return Err(invalid(
                    "Reviewed replacement requires an explicit Windows DACL",
                ));
            }
            let start = pointer as usize;
            let end = start + bytes;
            let acl_start = acl as usize;
            if acl_start < start
                || acl_start
                    .checked_add(size_of::<ACL>())
                    .is_none_or(|limit| limit > end)
            {
                return Err(invalid("Windows returned an invalid DACL location"));
            }
            if acl_start
                .checked_add(usize::from((*acl).AclSize))
                .is_none_or(|limit| limit > end)
                || IsValidAcl(acl) == 0
            {
                return Err(invalid("Windows returned an invalid replacement DACL"));
            }
            let mut information = ACL_SIZE_INFORMATION::default();
            check(GetAclInformation(
                acl,
                (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            ))?;
            let used = usize::try_from(information.AclBytesInUse).map_err(io::Error::other)?;
            if used < size_of::<ACL>() || used > usize::from((*acl).AclSize) {
                return Err(invalid("Windows returned an invalid DACL byte count"));
            }
            let mut owned = vec![0_u32; used.div_ceil(size_of::<u32>())];
            std::ptr::copy_nonoverlapping(acl.cast::<u8>(), owned.as_mut_ptr().cast(), used);
            // Free ACL padding has no access semantics. Normalize it so a
            // round trip is bound to the complete ordered ACE bytes.
            (*owned.as_mut_ptr().cast::<ACL>()).AclSize =
                u16::try_from(used).map_err(io::Error::other)?;
            Ok(Self {
                acl: owned,
                control: control & REVIEWED_CONTROL,
            })
        }
    }

    pub fn require_matches(&self, file: &File) -> io::Result<()> {
        if Self::capture(file)? == *self {
            Ok(())
        } else {
            Err(invalid(
                "The reviewed Windows DACL or inheritance control changed",
            ))
        }
    }

    pub fn require_matches_directory(&self, directory: &crate::tree::Directory) -> io::Result<()> {
        if Self::capture_directory(directory)? == *self {
            Ok(())
        } else {
            Err(invalid(
                "The reviewed Windows directory DACL or inheritance control changed",
            ))
        }
    }

    /// Copies the ACE sequence as explicit permissions using the Java-compatible
    /// legacy setter. The resulting DACL remains unprotected.
    /// Owner, group and other file metadata follow the new staged file.
    pub fn apply_to(&self, file: &File) -> io::Result<()> {
        require_regular(file)?;
        let acl = self.final_acl()?;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let pointer = from_mut(&mut descriptor).cast();
        // SAFETY: The descriptor and validated aligned ACL remain live for these
        // synchronous calls. The retained target handle grants WRITE_DAC. Like
        // OpenJDK's SetFileSecurityW, this supplies a freshly initialized DACL
        // descriptor without merging ACEs from the private staging parent.
        unsafe {
            check(InitializeSecurityDescriptor(
                pointer,
                SECURITY_DESCRIPTOR_REVISION,
            ))?;
            check(SetSecurityDescriptorDacl(
                pointer,
                1,
                acl.as_ptr().cast_mut().cast(),
                0,
            ))?;
            check(SetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                pointer,
            ))?;
        }
        self.require_applied(file)
    }

    /// Verifies the copied explicit ACE sequence and unprotected final DACL.
    pub fn require_applied(&self, file: &File) -> io::Result<()> {
        let applied = Self::capture(file)?;
        if applied.acl != self.final_acl()? || applied.control & SE_DACL_PROTECTED != 0 {
            return Err(invalid("Could not verify the replacement Windows DACL"));
        }
        Ok(())
    }

    /// Restores the exact native DACL, including inherited ACEs and control.
    /// This is used for ordinary creation, independently of Java's ACL setter.
    pub(crate) fn apply_native_to(&self, file: &File) -> io::Result<()> {
        self.apply_native_to_object(file, false)
    }

    pub(crate) fn apply_native_to_directory(
        &self,
        directory: &crate::tree::Directory,
    ) -> io::Result<()> {
        self.apply_native_to_object(directory.file(), true)
    }

    fn apply_native_to_object(&self, file: &File, directory: bool) -> io::Result<()> {
        require_object(file, directory)?;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let pointer = from_mut(&mut descriptor).cast();
        let control_mask = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ;
        // SAFETY: The descriptor and captured aligned ACL remain live through
        // synchronous calls. The staged handle grants WRITE_DAC and READ_CONTROL.
        unsafe {
            check(InitializeSecurityDescriptor(
                pointer,
                SECURITY_DESCRIPTOR_REVISION,
            ))?;
            check(SetSecurityDescriptorDacl(
                pointer,
                1,
                self.acl.as_ptr().cast_mut().cast(),
                i32::from(self.control & SE_DACL_DEFAULTED != 0),
            ))?;
            let control = self.control
                | if self.control & SE_DACL_AUTO_INHERITED != 0 {
                    SE_DACL_AUTO_INHERIT_REQ
                } else {
                    0
                };
            check(SetSecurityDescriptorControl(
                pointer,
                control_mask,
                control & control_mask,
            ))?;
            check(SetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                pointer,
            ))?;
        }
        if Self::capture_object(file, directory)? == *self {
            Ok(())
        } else {
            Err(invalid(
                "The native Windows DACL or inheritance control changed",
            ))
        }
    }

    fn final_acl(&self) -> io::Result<Vec<u32>> {
        let mut explicit = self.acl.clone();
        let pointer = explicit.as_mut_ptr().cast::<ACL>();
        // SAFETY: Capture validated every ACE in this aligned, owned ACL copy.
        // Java's AclEntry flags omit INHERITED_ACE when rebuilding the final list.
        unsafe {
            for index in 0..u32::from((*pointer).AceCount) {
                let mut entry = null_mut();
                check(GetAce(pointer, index, &mut entry))?;
                (*entry.cast::<ACE_HEADER>()).AceFlags &= !(INHERITED_ACE as u8);
            }
        }
        Ok(explicit)
    }
}

fn require_regular(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || crate::tree::is_reparse_point(&metadata)
        || crate::tree::file_link_count(file)? != 1
    {
        return Err(invalid("Replacement DACLs require one ordinary local file"));
    }
    Ok(())
}

fn require_object(file: &File, directory: bool) -> io::Result<()> {
    if !directory {
        return require_regular(file);
    }
    let metadata = file.metadata()?;
    if metadata.is_dir() && !crate::tree::is_reparse_point(&metadata) {
        Ok(())
    } else {
        Err(invalid(
            "Directory DACLs require one ordinary local directory",
        ))
    }
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
