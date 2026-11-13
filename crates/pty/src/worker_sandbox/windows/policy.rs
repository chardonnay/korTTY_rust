use std::env;
use std::ffi::{OsStr, c_void};
use std::fmt::Write as _;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    ACL, ACL_REVISION, AccessCheck, AddAccessAllowedAce, CreateWellKnownSid, DuplicateToken,
    EqualSid, FreeSid, GENERIC_MAPPING, GetTokenInformation, InitializeAcl,
    InitializeSecurityDescriptor, PRIVILEGE_SET, PSID, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SECURITY_MAX_SID_SIZE, SecurityImpersonation, SetSecurityDescriptorDacl,
    SetSecurityDescriptorGroup, SetSecurityDescriptorOwner, TOKEN_APPCONTAINER_INFORMATION,
    TOKEN_DUPLICATE, TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER,
    TokenAppContainerSid, TokenCapabilities, TokenIsAppContainer, TokenUser, WELL_KNOWN_SID_TYPE,
    WinBuiltinAnyPackageSid, WinWorldSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_DATA,
};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows_sys::Win32::System::SystemServices::{
    PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessMitigationPolicy, OpenProcessToken, ProcessSystemCallDisablePolicy,
};

use super::{bool_result, wide};

pub(super) struct Environment {
    pub(super) system_root: Vec<u16>,
    pub(super) block: Vec<u16>,
}

impl Environment {
    pub(super) fn read() -> io::Result<Self> {
        let mut system_root = vec![0_u16; 32_768];
        // SAFETY: the Windows API receives a live writable WCHAR buffer.
        let length =
            unsafe { GetWindowsDirectoryW(system_root.as_mut_ptr(), system_root.len() as u32) }
                as usize;
        if length == 0 || length >= system_root.len() {
            return Err(io::Error::other(
                "could not find the Windows system directory",
            ));
        }
        system_root.truncate(length + 1);
        let mut block = Vec::new();
        // Windows rehomes the sandbox's local and temporary directories when
        // creating an AppContainer. Keep the inputs necessary for this, in
        // case-insensitive order, and exclude the desktop's other variables.
        for key in [
            "APPDATA",
            "LOCALAPPDATA",
            "SystemRoot",
            "TEMP",
            "TMP",
            "USERPROFILE",
        ] {
            if key == "SystemRoot" {
                block.extend(key.encode_utf16().chain([b'=' as u16]));
                block.extend_from_slice(&system_root);
            } else if let Some(value) =
                env::var_os(key).filter(|value| Path::new(value).is_absolute())
            {
                block.extend(key.encode_utf16().chain([b'=' as u16]));
                block.extend(value.encode_wide().chain([0]));
            } else if key == "LOCALAPPDATA" || key == "USERPROFILE" {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Windows SSH sandbox requires a local user profile",
                ));
            }
        }
        block.push(0);
        Ok(Self { system_root, block })
    }
}

pub(super) struct Profile(Vec<u16>);

impl Profile {
    pub(super) fn create() -> io::Result<(Self, AppSid)> {
        let mut random = [0_u8; 16];
        // SAFETY: the initialized array is live and the default RNG needs no handle.
        let status = unsafe {
            BCryptGenRandom(
                null_mut(),
                random.as_mut_ptr(),
                random.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "could not name the SSH AppContainer ({status:#x})"
            )));
        }
        let mut name = String::from("kortty.ssh.");
        for byte in random {
            let _ = write!(name, "{byte:02x}");
        }
        let name = wide(OsStr::new(&name));
        let mut sid = null_mut();
        // SAFETY: all three strings are NUL terminated and the SID output is writable.
        let status = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if status != 0 {
            return Err(io::Error::other(format!(
                "could not create a fresh SSH AppContainer ({status:#x})"
            )));
        }
        let profile = Self(name);
        if sid.is_null() {
            return Err(io::Error::other("Windows returned no SSH AppContainer SID"));
        }
        Ok((profile, AppSid(sid)))
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // SAFETY: this guard created this uniquely named profile.
        unsafe { DeleteAppContainerProfile(self.0.as_ptr()) };
    }
}

pub(super) struct AppSid(PSID);

impl AppSid {
    pub(super) fn raw(&self) -> PSID {
        self.0
    }
}

impl Drop for AppSid {
    fn drop(&mut self) {
        // SAFETY: CreateAppContainerProfile transferred this SID allocation.
        unsafe { FreeSid(self.0) };
    }
}

pub(super) fn verify(process: HANDLE, expected_sid: Option<PSID>) -> io::Result<()> {
    let token = open_token(process, TOKEN_QUERY | TOKEN_DUPLICATE)?;
    if token_info::<u32>(&token, TokenIsAppContainer)? != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH worker is outside its AppContainer",
        ));
    }
    let mut sid_storage = [0_usize; 16];
    token_read(
        &token,
        TokenAppContainerSid,
        sid_storage.as_mut_ptr().cast(),
        size_of_val(&sid_storage) as u32,
    )?;
    // SAFETY: GetTokenInformation filled the aligned TOKEN_APPCONTAINER_INFORMATION.
    let container = unsafe {
        &*sid_storage
            .as_ptr()
            .cast::<TOKEN_APPCONTAINER_INFORMATION>()
    };
    if container.TokenAppContainer.is_null()
        || expected_sid.is_some_and(|sid| {
            // SAFETY: both SIDs belong to still-live token output or to the launcher.
            unsafe { EqualSid(sid, container.TokenAppContainer) == 0 }
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH worker has an unexpected AppContainer identity",
        ));
    }
    let groups = token_info::<TOKEN_GROUPS>(&token, TokenCapabilities)?;
    if groups.GroupCount != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH worker has unexpected AppContainer capabilities",
        ));
    }
    verify_lpac(&token, container.TokenAppContainer)?;
    verify_win32k_disabled(process)
}

fn verify_win32k_disabled(process: HANDLE) -> io::Result<()> {
    let mut policy = PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY::default();
    // SAFETY: process is live and this initialized output has the documented type and size.
    bool_result(unsafe {
        GetProcessMitigationPolicy(
            process,
            ProcessSystemCallDisablePolicy,
            std::ptr::from_mut(&mut policy).cast(),
            size_of_val(&policy),
        )
    })?;
    // SAFETY: Flags is the DWORD view of the initialized policy. Bit zero is
    // DisallowWin32kSystemCalls in PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY.
    if unsafe { policy.Anonymous.Flags } & 1 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH worker can call Win32k",
        ));
    }
    Ok(())
}

pub(crate) fn with_private_pipe<T>(
    create: impl FnOnce(&SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    // SAFETY: GetCurrentProcess returns the current process pseudo-handle.
    let token = open_token(unsafe { GetCurrentProcess() }, TOKEN_QUERY)?;
    let mut sid_storage = [0_usize; 24];
    token_read(
        &token,
        TokenUser,
        sid_storage.as_mut_ptr().cast(),
        size_of_val(&sid_storage) as u32,
    )?;
    // SAFETY: GetTokenInformation filled the aligned TOKEN_USER and its SID.
    let user = unsafe { &*sid_storage.as_ptr().cast::<TOKEN_USER>() }
        .User
        .Sid;
    if user.is_null() {
        return Err(io::Error::other(
            "could not identify the launcher for the SSH relay pipe",
        ));
    }
    let mut acl = [0_u32; 32];
    let acl_size = size_of_val(&acl) as u32;
    let acl = acl.as_mut_ptr().cast::<ACL>();
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor = std::ptr::from_mut(&mut descriptor).cast();
    // SAFETY: all pointers refer to live buffers of the documented types. The
    // launcher-only ACL and SID outlive the descriptor and CreateNamedPipe call.
    unsafe {
        bool_result(InitializeAcl(acl, acl_size, ACL_REVISION))?;
        bool_result(AddAccessAllowedAce(
            acl,
            ACL_REVISION,
            FILE_ALL_ACCESS,
            user,
        ))?;
        bool_result(InitializeSecurityDescriptor(
            descriptor,
            SECURITY_DESCRIPTOR_REVISION,
        ))?;
        bool_result(SetSecurityDescriptorDacl(descriptor, 1, acl, 0))?;
    }
    create(&SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    })
}

fn open_token(process: HANDLE, access: u32) -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    // SAFETY: process is a live process handle and the output is writable.
    bool_result(unsafe { OpenProcessToken(process, access, &mut token) })?;
    // SAFETY: OpenProcessToken transferred the returned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

fn verify_lpac(token: &OwnedHandle, package: PSID) -> io::Result<()> {
    let mut impersonation = null_mut();
    // SAFETY: AccessCheck requires an impersonation token. We never install it
    // as a thread token, and this output handle belongs only to the check.
    bool_result(unsafe {
        DuplicateToken(
            token.as_raw_handle(),
            SecurityImpersonation,
            &mut impersonation,
        )
    })?;
    // SAFETY: successful DuplicateToken transferred a new owned handle.
    let impersonation = unsafe { OwnedHandle::from_raw_handle(impersonation) };
    let mut everyone = WellKnownSid::new(WinWorldSid)?;
    let mut all_packages = WellKnownSid::new(WinBuiltinAnyPackageSid)?;
    let everyone = everyone.raw();
    // An exact package grant must pass as a positive control. An LPAC opts out
    // of the All Application Packages grant that a regular AppContainer uses.
    if !check_file_access(&impersonation, everyone, package)?
        || check_file_access(&impersonation, everyone, all_packages.raw())?
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH worker is outside its less-privileged AppContainer",
        ));
    }
    Ok(())
}

struct WellKnownSid([u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<u32>())]);

impl WellKnownSid {
    fn new(kind: WELL_KNOWN_SID_TYPE) -> io::Result<Self> {
        let mut storage = Self([0; (SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<u32>())]);
        let mut size = size_of::<Self>() as u32;
        // SAFETY: this SID class requires no domain SID. Storage is aligned and
        // large enough for a Windows SID.
        bool_result(unsafe { CreateWellKnownSid(kind, null_mut(), storage.raw(), &mut size) })?;
        Ok(storage)
    }

    fn raw(&mut self) -> PSID {
        self.0.as_mut_ptr().cast()
    }
}

fn check_file_access(token: &OwnedHandle, everyone: PSID, subject: PSID) -> io::Result<bool> {
    // Space for an ACL header and two access-allowed ACEs with maximum-length SIDs.
    let mut acl = [0_u32; 64];
    let acl_size = size_of_val(&acl) as u32;
    let acl = acl.as_mut_ptr().cast::<ACL>();
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor = std::ptr::from_mut(&mut descriptor).cast();
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    let mut privileges = PRIVILEGE_SET::default();
    let mut size = size_of_val(&privileges) as u32;
    let mut granted = 0;
    let mut allowed = 0;
    // SAFETY: all pointers refer to live buffers of the documented types. The
    // ACL and owner/group SIDs outlive the absolute security descriptor.
    unsafe {
        bool_result(InitializeAcl(acl, acl_size, ACL_REVISION))?;
        bool_result(AddAccessAllowedAce(
            acl,
            ACL_REVISION,
            FILE_READ_DATA,
            everyone,
        ))?;
        bool_result(AddAccessAllowedAce(
            acl,
            ACL_REVISION,
            FILE_READ_DATA,
            subject,
        ))?;
        bool_result(InitializeSecurityDescriptor(
            descriptor,
            SECURITY_DESCRIPTOR_REVISION,
        ))?;
        bool_result(SetSecurityDescriptorOwner(descriptor, everyone, 0))?;
        bool_result(SetSecurityDescriptorGroup(descriptor, everyone, 0))?;
        bool_result(SetSecurityDescriptorDacl(descriptor, 1, acl, 0))?;
        bool_result(AccessCheck(
            descriptor,
            token.as_raw_handle(),
            FILE_READ_DATA,
            &mapping,
            &mut privileges,
            &mut size,
            &mut granted,
            &mut allowed,
        ))?;
    }
    Ok(allowed != 0 && granted & FILE_READ_DATA != 0)
}

fn token_info<T: Default>(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<T> {
    let mut value = T::default();
    token_read(
        token,
        class,
        std::ptr::from_mut(&mut value).cast(),
        size_of::<T>() as u32,
    )?;
    Ok(value)
}

fn token_read(
    token: &OwnedHandle,
    class: TOKEN_INFORMATION_CLASS,
    output: *mut c_void,
    capacity: u32,
) -> io::Result<()> {
    let mut used = 0;
    // SAFETY: each private caller passes the correctly aligned writable buffer
    // for its class. GetTokenInformation checks the capacity before writing.
    bool_result(unsafe {
        GetTokenInformation(token.as_raw_handle(), class, output, capacity, &mut used)
    })
}
