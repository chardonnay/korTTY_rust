/// Host name reported by the local OS, without a network lookup.
#[cfg(unix)]
pub fn local_hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    // SAFETY: the buffer is writable for the byte count passed to gethostname.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let end = buffer.iter().position(|byte| *byte == 0)?;
    (!buffer[..end].is_empty()).then(|| String::from_utf8_lossy(&buffer[..end]).into_owned())
}

/// Account name for the local process's real user ID.
#[cfg(unix)]
pub fn local_username() -> Option<String> {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    let mut buffer = vec![0_u8; 1024];
    let mut account = MaybeUninit::<libc::passwd>::uninit();
    loop {
        let mut result = std::ptr::null_mut();
        // SAFETY: the account, data buffer and result pointer remain live for the call.
        // getpwuid_r borrows the caller's buffer until the result has been read.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                account.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 64 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: success with a non-null result initializes the passwd record.
        let name = unsafe { (*result).pw_name };
        if name.is_null() {
            return None;
        }
        // SAFETY: pw_name is NUL-terminated and the backing buffer is still live.
        let name = unsafe { CStr::from_ptr(name) };
        return (!name.is_empty()).then(|| name.to_string_lossy().into_owned());
    }
}

/// DNS host label configured on the local Windows computer.
#[cfg(windows)]
pub fn local_hostname() -> Option<String> {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNameDnsHostname, GetComputerNameExW,
    };

    let mut buffer = [0_u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: buffer and size are writable; size is its UTF-16 capacity.
    let result =
        unsafe { GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size) };
    if result == 0 || size == 0 {
        return None;
    }
    String::from_utf16(buffer.get(..size as usize)?).ok()
}

/// Local Windows logon account name.
#[cfg(windows)]
pub fn local_username() -> Option<String> {
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;

    let mut buffer = [0_u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: buffer and size are writable; size is its UTF-16 capacity.
    let result = unsafe { GetUserNameW(buffer.as_mut_ptr(), &mut size) };
    if result == 0 || size <= 1 {
        return None;
    }
    // The returned length includes the final NUL.
    String::from_utf16(buffer.get(..size as usize - 1)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_identity_provides_the_local_account_and_host() {
        for value in [local_username(), local_hostname()] {
            let value = value.expect("native test host exposes its account and host name");
            assert!(!value.trim().is_empty());
            assert!(!value.contains('\0'));
        }
    }
}
