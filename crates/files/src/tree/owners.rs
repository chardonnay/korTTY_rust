//! Read-only Windows file owner labels through an anchored directory.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Wdk::Storage::FileSystem::FILE_OPEN;
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetLengthSid, IsValidSid, LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSID,
    SECURITY_MAX_SID_SIZE,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_READ_ATTRIBUTES, READ_CONTROL, SYNCHRONIZE};

use super::{Directory, is_reparse_point};

const MAX_ACCOUNT_UNITS: u32 = 1024;
const MAX_SID_UNITS: usize = 256;

/// Resolves each distinct owner SID once while enriching a directory listing.
pub struct FileOwnerNames {
    directory: Directory,
    names: HashMap<Vec<u8>, Option<String>>,
}

impl FileOwnerNames {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            directory: Directory::open_path(path)?,
            names: HashMap::new(),
        })
    }

    /// Requests only attributes and security information for one ordinary child.
    pub fn owner(&mut self, name: &OsStr) -> io::Result<Option<String>> {
        let file = self.directory.open(
            name,
            FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
            FILE_OPEN,
            0,
            None,
        )?;
        if is_reparse_point(&file.metadata()?) {
            return Ok(None);
        }
        let mut owner = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: The handle is open. Both requested output pointers are live;
        // Win32 returns an allocated descriptor containing the owner SID.
        let result = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        let descriptor = LocalAllocation(descriptor);
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        // SAFETY: The successful call returned a SID into the live descriptor.
        if descriptor.0.is_null() || owner.is_null() || unsafe { IsValidSid(owner) } == 0 {
            return Ok(None);
        }
        // SAFETY: The owner SID is valid and its descriptor remains live.
        let length = unsafe { GetLengthSid(owner) };
        if !(1..=SECURITY_MAX_SID_SIZE).contains(&length) {
            return Ok(None);
        }
        // SAFETY: The valid SID occupies exactly this many bytes.
        let key = unsafe { std::slice::from_raw_parts(owner.cast::<u8>(), length as usize) };
        if let Some(label) = self.names.get(key) {
            return Ok(label.clone());
        }
        let label = account_name(owner).or_else(|| sid_name(owner));
        self.names.insert(key.to_vec(), label.clone());
        Ok(label)
    }
}

struct LocalAllocation(HLOCAL);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The pointer came from a Win32 API that requires LocalFree.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn account_name(sid: PSID) -> Option<String> {
    let mut name_len = 0;
    let mut domain_len = 0;
    let mut kind = 0;
    // SAFETY: The SID is valid for both calls. Null output buffers request their sizes.
    let result = unsafe {
        LookupAccountSidW(
            null(),
            sid,
            null_mut(),
            &mut name_len,
            null_mut(),
            &mut domain_len,
            &mut kind,
        )
    };
    if result != 0
        || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        || !(1..=MAX_ACCOUNT_UNITS).contains(&name_len)
        || domain_len > MAX_ACCOUNT_UNITS
    {
        return None;
    }
    let mut name = vec![0; name_len as usize];
    let mut domain = vec![0; domain_len as usize];
    let domain_buffer = if domain.is_empty() {
        null_mut()
    } else {
        domain.as_mut_ptr()
    };
    // SAFETY: The output buffers are writable for their respective capacities.
    let result = unsafe {
        LookupAccountSidW(
            null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain_buffer,
            &mut domain_len,
            &mut kind,
        )
    };
    if result == 0 {
        return None;
    }
    let name = decode_name(&name);
    let domain = decode_name(&domain);
    if name.is_empty() {
        return None;
    }
    if domain.is_empty() {
        Some(name)
    } else {
        Some(format!("{domain}\\{name}"))
    }
}

fn decode_name(units: &[u16]) -> String {
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

fn sid_name(sid: PSID) -> Option<String> {
    let mut text = null_mut();
    // SAFETY: The SID is valid and the pointer receives a NUL-terminated allocation.
    let result = unsafe { ConvertSidToStringSidW(sid, &mut text) };
    let allocation = LocalAllocation(text.cast());
    if result == 0 || allocation.0.is_null() {
        return None;
    }
    for end in 0..MAX_SID_UNITS {
        // SAFETY: The returned string lives in allocation. The loop stops at its NUL.
        if unsafe { *text.add(end) } == 0 {
            // SAFETY: The terminator proves that these preceding units are allocated.
            return Some(String::from_utf16_lossy(unsafe {
                std::slice::from_raw_parts(text, end)
            }));
        }
    }
    None
}
