use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::slice;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NONE_MAPPED};
use windows_sys::Win32::Security::{
    GetTokenInformation, LookupAccountSidW, PSID, SECURITY_APP_PACKAGE_AUTHORITY,
    SECURITY_AUTHENTICATION_AUTHORITY, SECURITY_MANDATORY_LABEL_AUTHORITY, SECURITY_NT_AUTHORITY,
    SID, SID_AND_ATTRIBUTES, TOKEN_GROUPS, TOKEN_QUERY, TokenGroups,
};
use windows_sys::Win32::System::SystemServices::{
    SE_GROUP_LOGON_ID, SECURITY_APP_PACKAGE_BASE_RID, SECURITY_CAPABILITY_BASE_RID,
    SECURITY_LOGON_IDS_RID, SECURITY_LOGON_IDS_RID_COUNT, SID_MAX_SUB_AUTHORITIES, SID_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{MAX_GROUPS, valid_group_name};

const MAX_TOKEN_BYTES: usize = 1024 * 1024;
const MAX_ACCOUNT_UNITS: u32 = 1024;
const GROUPS_OFFSET: usize = offset_of!(TOKEN_GROUPS, Groups);
const SID_PREFIX: usize = offset_of!(SID, SubAuthority);

pub(super) fn names() -> io::Result<Vec<String>> {
    let token = process_token()?;
    let (buffer, used) = read_group_token(&token)?;
    // SAFETY: The initialized, aligned buffer remains live throughout group processing.
    let bytes = unsafe { slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), used) };
    let count = u32::from_ne_bytes(bytes[..size_of::<u32>()].try_into().unwrap()) as usize;
    if count > MAX_GROUPS || GROUPS_OFFSET + count * size_of::<SID_AND_ATTRIBUTES>() > used {
        return Err(invalid_token());
    }
    // SAFETY: The offset is aligned by TOKEN_GROUPS's layout, and the count fits the buffer.
    let groups = unsafe {
        slice::from_raw_parts(
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(GROUPS_OFFSET)
                .cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    let mut names = Vec::with_capacity(count);
    for group in groups {
        let sid = sid_bytes(bytes, group.Sid)?;
        if group.Attributes & SE_GROUP_LOGON_ID as u32 == SE_GROUP_LOGON_ID as u32 {
            // A per-logon token identifier is not a directory or local account group.
            continue;
        }
        let (name, domain) = match account_name(group.Sid) {
            Ok(account) => account,
            Err(error)
                if error.raw_os_error() == Some(ERROR_NONE_MAPPED as i32)
                    && ignorable_unmapped_sid(sid) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if !domain.is_empty() {
            let qualified = format!("{domain}\\{name}");
            if !valid_group_name(&qualified) {
                return Err(io::Error::other("Invalid qualified group account name"));
            }
            names.push(qualified);
        }
        names.push(name);
    }
    Ok(names)
}

fn process_token() -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    // SAFETY: The process pseudo-handle is valid and token is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned an owned handle for this process's token.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

fn read_group_token(token: &OwnedHandle) -> io::Result<(Vec<usize>, usize)> {
    let mut required = 0;
    // SAFETY: The zero-sized probe only writes the required byte count.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenGroups,
            null_mut(),
            0,
            &mut required,
        )
    } != 0
    {
        return Err(invalid_token());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(error);
    }
    if !(GROUPS_OFFSET..=MAX_TOKEN_BYTES).contains(&(required as usize)) {
        return Err(invalid_token());
    }
    let mut buffer = vec![0_usize; (required as usize).div_ceil(size_of::<usize>())];
    let mut used = 0;
    // SAFETY: The aligned buffer has at least required bytes and is writable.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenGroups,
            buffer.as_mut_ptr().cast(),
            required,
            &mut used,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if !(GROUPS_OFFSET..=required as usize).contains(&(used as usize)) {
        return Err(invalid_token());
    }
    Ok((buffer, used as usize))
}

fn sid_bytes(buffer: &[u8], sid: PSID) -> io::Result<&[u8]> {
    let offset = sid
        .addr()
        .checked_sub(buffer.as_ptr().addr())
        .ok_or_else(invalid_token)?;
    let data = buffer.get(offset..).ok_or_else(invalid_token)?;
    let [version, count, ..] = data else {
        return Err(invalid_token());
    };
    if u32::from(*version) != SID_REVISION || u32::from(*count) > SID_MAX_SUB_AUTHORITIES {
        return Err(invalid_token());
    }
    let len = SID_PREFIX + usize::from(*count) * size_of::<u32>();
    data.get(..len).ok_or_else(invalid_token)
}

fn ignorable_unmapped_sid(sid: &[u8]) -> bool {
    let authority = &sid[2..SID_PREFIX];
    let count = sid[1];
    let first = sid
        .get(SID_PREFIX..SID_PREFIX + size_of::<u32>())
        .map(|bytes| i32::from_le_bytes(bytes.try_into().unwrap()));
    // Unnamed security labels, capabilities and logon identifiers are not account groups.
    // Do not omit ordinary NT account SIDs, even when Windows returns ERROR_NONE_MAPPED.
    (authority == SECURITY_MANDATORY_LABEL_AUTHORITY.Value && count == 1)
        || (authority == SECURITY_AUTHENTICATION_AUTHORITY.Value && count == 1)
        || (authority == SECURITY_APP_PACKAGE_AUTHORITY.Value
            && matches!(
                first,
                Some(SECURITY_APP_PACKAGE_BASE_RID | SECURITY_CAPABILITY_BASE_RID)
            ))
        || (authority == SECURITY_NT_AUTHORITY.Value
            && i32::from(count) == SECURITY_LOGON_IDS_RID_COUNT
            && first == Some(SECURITY_LOGON_IDS_RID))
}

fn account_name(sid: PSID) -> io::Result<(String, String)> {
    let mut name_len = 0;
    let mut domain_len = 0;
    let mut kind = 0;
    // SAFETY: The SID was checked inside the live token buffer. Null buffers request sizes.
    if unsafe {
        LookupAccountSidW(
            null(),
            sid,
            null_mut(),
            &mut name_len,
            null_mut(),
            &mut domain_len,
            &mut kind,
        )
    } != 0
    {
        return Err(invalid_token());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(error);
    }
    if !(1..=MAX_ACCOUNT_UNITS).contains(&name_len) || domain_len > MAX_ACCOUNT_UNITS {
        return Err(io::Error::other(
            "Local group account name exceeds its size limit",
        ));
    }
    let mut name = vec![0; name_len as usize];
    let mut domain = vec![0; domain_len as usize];
    let domain_buffer = if domain.is_empty() {
        null_mut()
    } else {
        domain.as_mut_ptr()
    };
    // SAFETY: Both optional output buffers are writable for the UTF-16 capacities supplied.
    if unsafe {
        LookupAccountSidW(
            null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain_buffer,
            &mut domain_len,
            &mut kind,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let name = decode_name(&name)?;
    let domain = if domain.is_empty() {
        String::new()
    } else {
        decode_name(&domain)?
    };
    if !valid_group_name(&name) || (!domain.is_empty() && !valid_group_name(&domain)) {
        return Err(io::Error::other("Invalid local group account name"));
    }
    Ok((name, domain))
}

fn decode_name(units: &[u16]) -> io::Result<String> {
    units
        .iter()
        .position(|&unit| unit == 0)
        .and_then(|end| String::from_utf16(&units[..end]).ok())
        .ok_or_else(|| io::Error::other("Invalid local group account name"))
}

fn invalid_token() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid local group token")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(authority: u8, subauthorities: &[u32]) -> Vec<u8> {
        let mut sid = vec![
            SID_REVISION as u8,
            subauthorities.len() as u8,
            0,
            0,
            0,
            0,
            0,
            authority,
        ];
        sid.extend(subauthorities.iter().flat_map(|rid| rid.to_le_bytes()));
        sid
    }

    #[test]
    fn only_unmapped_token_labels_and_non_account_ids_can_be_ignored() {
        for value in [
            sid(15, &[3, 1]),
            sid(16, &[8192]),
            sid(18, &[1]),
            sid(5, &[5, 1, 2]),
        ] {
            assert!(ignorable_unmapped_sid(&value));
        }
        for value in [sid(5, &[21, 1, 2, 3, 4]), sid(5, &[32, 544]), sid(5, &[11])] {
            assert!(!ignorable_unmapped_sid(&value));
        }
    }
}
