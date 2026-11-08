use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::ptr::{from_mut, null_mut};

use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, GetKernelObjectSecurity, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetSidLengthRequired,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, IsValidAcl, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR,
    SECURITY_MAX_SID_SIZE, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::ok;

pub(super) fn with_private_descriptor<T>(
    create: impl FnOnce(&SECURITY_DESCRIPTOR, PSID) -> io::Result<T>,
) -> io::Result<T> {
    let owner = UserSid::current()?;
    let sid = owner.sid();
    // One ACL header and one ACE fit even with a maximum-length Windows SID.
    let mut acl = [0_u32; 32];
    let length = size_of_val(&acl) as u32;
    let acl = acl.as_mut_ptr().cast::<ACL>();
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let pointer = from_mut(&mut descriptor).cast();
    // SAFETY: The aligned ACL backing storage and valid TokenUser SID remain live
    // through construction and the synchronous object creation closure.
    unsafe {
        ok(InitializeAcl(acl, length, ACL_REVISION))?;
        ok(AddAccessAllowedAceEx(
            acl,
            ACL_REVISION,
            0,
            FILE_ALL_ACCESS,
            sid,
        ))?;
        ok(InitializeSecurityDescriptor(
            pointer,
            SECURITY_DESCRIPTOR_REVISION,
        ))?;
        ok(SetSecurityDescriptorOwner(pointer, sid, 0))?;
        ok(SetSecurityDescriptorDacl(pointer, 1, acl, 0))?;
        ok(SetSecurityDescriptorControl(
            pointer,
            SE_DACL_PROTECTED,
            SE_DACL_PROTECTED,
        ))?;
    }
    create(&descriptor, sid)
}

pub(super) fn require_owner_only(file: &File, sid: PSID) -> io::Result<()> {
    let mut bytes = [0_u64; 64];
    let size = size_of_val(&bytes) as u32;
    let descriptor = bytes.as_mut_ptr().cast();
    let mut length = 0;
    let (mut owner, mut acl, mut ace) = (null_mut(), null_mut(), null_mut());
    let (mut present, mut defaulted, mut control, mut revision) = (0, 0, 0, 0);
    // SAFETY: The output buffer and all output fields are writable. After validating
    // the descriptor and ACL, Win32's pointers are into that live output buffer.
    unsafe {
        ok(GetKernelObjectSecurity(
            file.as_raw_handle(),
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            descriptor,
            size,
            &mut length,
        ))?;
        ok(GetSecurityDescriptorControl(
            descriptor,
            &mut control,
            &mut revision,
        ))?;
        ok(GetSecurityDescriptorOwner(
            descriptor,
            &mut owner,
            &mut defaulted,
        ))?;
        ok(GetSecurityDescriptorDacl(
            descriptor,
            &mut present,
            &mut acl,
            &mut defaulted,
        ))?;
        if control & SE_DACL_PROTECTED == 0
            || owner.is_null()
            || EqualSid(owner, sid) == 0
            || present == 0
            || acl.is_null()
            || IsValidAcl(acl) == 0
            || (*acl).AceCount != 1
        {
            return Err(not_private());
        }
        ok(GetAce(acl, 0, &mut ace))?;
        if ace.is_null() {
            return Err(not_private());
        }
        let header = &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>();
        let offset = offset_of!(ACCESS_ALLOWED_ACE, SidStart);
        let sid_bytes = usize::from(header.AceSize).saturating_sub(offset);
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
            || header.AceFlags != 0
            || sid_bytes < 8
        {
            return Err(not_private());
        }
        let trustee = ace.cast::<u8>().add(offset);
        if GetSidLengthRequired(*trustee.add(1)) as usize > sid_bytes
            || IsValidSid(trustee.cast()) == 0
            || EqualSid(trustee.cast(), sid) == 0
        {
            return Err(not_private());
        }
    }
    Ok(())
}

pub(super) fn require_current_user_only(file: &File) -> io::Result<()> {
    let owner = UserSid::current()?;
    require_owner_only(file, owner.sid())
}

fn not_private() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Windows did not preserve private folder permissions",
    )
}

#[repr(C)]
struct UserSid {
    user: TOKEN_USER,
    data: [u8; SECURITY_MAX_SID_SIZE as usize],
}

impl UserSid {
    fn current() -> io::Result<Box<Self>> {
        let mut token = null_mut();
        // SAFETY: GetCurrentProcess supplies its pseudo-handle and token is writable.
        ok(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
        // SAFETY: OpenProcessToken returned a distinct owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut record = Box::new(Self {
            user: TOKEN_USER::default(),
            data: [0; SECURITY_MAX_SID_SIZE as usize],
        });
        let mut length = 0;
        // SAFETY: The aligned heap storage holds the TOKEN_USER plus a maximum SID
        // and stays at the same address while its interior pointer is used.
        ok(unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                from_mut(record.as_mut()).cast(),
                size_of::<Self>() as u32,
                &mut length,
            )
        })?;
        // SAFETY: A successful TokenUser query initialized its SID in the live record.
        if record.sid().is_null() || unsafe { IsValidSid(record.sid()) } == 0 {
            return Err(io::Error::other(
                "Windows did not return the current user SID",
            ));
        }
        Ok(record)
    }

    fn sid(&self) -> PSID {
        self.user.User.Sid
    }
}

#[cfg(test)]
mod tests;
