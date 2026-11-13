use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;
use std::ptr::null_mut;

use super::{MAX_GROUPS, valid_group_name};

const MAX_GROUP_RECORD_BYTES: usize = 64 * 1024;

pub(super) fn names() -> io::Result<Vec<String>> {
    let mut buffer = vec![0; 1024];
    group_ids()?
        .into_iter()
        .map(|id| group_name(id, &mut buffer))
        .collect()
}

fn group_ids() -> io::Result<Vec<libc::gid_t>> {
    for _ in 0..3 {
        // SAFETY: A size of zero asks for the count without writing through the pointer.
        let count = unsafe { libc::getgroups(0, null_mut()) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count as usize > MAX_GROUPS {
            return Err(io::Error::other("Too many local group memberships"));
        }
        let mut ids = vec![0; count as usize];
        // SAFETY: The buffer has space for count group IDs. Zero still requests a count.
        let received = unsafe { libc::getgroups(count, ids.as_mut_ptr()) };
        if received < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINVAL) {
                continue;
            }
            return Err(error);
        }
        if received > count {
            continue;
        }
        ids.truncate(received as usize);
        // SAFETY: These calls take no pointers and read the process's group IDs.
        ids.extend(unsafe { [libc::getgid(), libc::getegid()] });
        ids.sort_unstable();
        ids.dedup();
        return Ok(ids);
    }
    Err(io::Error::other(
        "Local group memberships changed during lookup",
    ))
}

fn group_name(id: libc::gid_t, buffer: &mut Vec<u8>) -> io::Result<String> {
    let mut record = MaybeUninit::<libc::group>::uninit();
    loop {
        let mut result = null_mut();
        // SAFETY: All output storage is valid, writable and live until the result is copied.
        let status = unsafe {
            libc::getgrgid_r(
                id,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < MAX_GROUP_RECORD_BYTES {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        if result.is_null() {
            return Err(io::Error::other(
                "A local group membership has no account name",
            ));
        }
        // SAFETY: Success with a non-null result initializes the record and its name pointer.
        let name = unsafe { (*result).gr_name };
        if name.is_null() {
            return Err(io::Error::other(
                "A local group membership has no account name",
            ));
        }
        // SAFETY: getgrgid_r returned a NUL-terminated name in the live caller-owned buffer.
        let name = unsafe { CStr::from_ptr(name) }.to_str().ok();
        return name
            .filter(|name| valid_group_name(name))
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("Invalid local group account name"));
    }
}
