//! Native Unix account lookups for ownership reviews and display labels.

use std::ffi::{CStr, CString, c_char, c_int};
use std::io;
use std::mem::MaybeUninit;
use std::ptr::null_mut;

use zeroize::{Zeroize as _, Zeroizing};

const MAX_NAME_BYTES: usize = 256;
const INITIAL_RECORD_BYTES: usize = 1024;
const MAX_RECORD_BYTES: usize = 256 * 1024;

type Lookup<Key, Record> =
    unsafe extern "C" fn(Key, *mut Record, *mut c_char, usize, *mut *mut Record) -> c_int;

/// Which Unix account database supplies the ownership ID.
pub enum Kind {
    User,
    Group,
}

/// Resolve an exact user or group name in the system account database.
pub fn resolve(kind: Kind, name: &str, guard: &impl Fn() -> io::Result<()>) -> io::Result<u32> {
    let invalid_name = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Enter a valid local account or group name.",
        )
    };
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(invalid_name());
    }
    let name = CString::new(name).map_err(|_| invalid_name())?;
    match kind {
        Kind::User => lookup(
            &name,
            libc::getpwnam_r,
            |record| (record.pw_name.cast_const(), record.pw_uid),
            guard,
        ),
        Kind::Group => lookup(
            &name,
            libc::getgrnam_r,
            |record| (record.gr_name.cast_const(), record.gr_gid),
            guard,
        ),
    }
}

fn lookup<Record>(
    name: &CStr,
    get: Lookup<*const c_char, Record>,
    fields: impl Fn(&Record) -> (*const c_char, u32),
    guard: &impl Fn() -> io::Result<()>,
) -> io::Result<u32> {
    lookup_record(
        name.as_ptr(),
        get,
        |buffer, record| {
            let (returned_name, id) = fields(record);
            if id == u32::MAX || !exact_name(buffer, returned_name, name) {
                return Err(invalid_record());
            }
            Ok(id)
        },
        "The exact local account or group name was not found.",
        guard,
    )
}

/// Looks up the label of a known UID or GID without changing its identity.
pub fn display_name(
    kind: Kind,
    id: u32,
    guard: &impl Fn() -> io::Result<()>,
) -> io::Result<String> {
    const MISSING: &str = "The local account or group ID was not found.";
    if id == u32::MAX {
        return Err(io::Error::new(io::ErrorKind::NotFound, MISSING));
    }
    match kind {
        Kind::User => lookup_record(
            id,
            libc::getpwuid_r,
            |buffer, record| display_label(buffer, record.pw_name.cast_const(), record.pw_uid, id),
            MISSING,
            guard,
        ),
        Kind::Group => lookup_record(
            id,
            libc::getgrgid_r,
            |buffer, record| display_label(buffer, record.gr_name.cast_const(), record.gr_gid, id),
            MISSING,
            guard,
        ),
    }
}

fn lookup_record<Key: Copy, Record, Output>(
    key: Key,
    get: Lookup<Key, Record>,
    accept: impl Fn(&[u8], &Record) -> io::Result<Output>,
    not_found: &'static str,
    guard: &impl Fn() -> io::Result<()>,
) -> io::Result<Output> {
    let mut buffer = Zeroizing::new(vec![0_u8; INITIAL_RECORD_BYTES]);
    let mut record = MaybeUninit::<Record>::uninit();
    loop {
        guard()?;
        let mut result = null_mut();
        // SAFETY: The caller provides a numeric ID or a live NUL-terminated
        // name. The writable record, buffer and result live through the call.
        let status = unsafe {
            get(
                key,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        guard()?;
        if status == libc::ERANGE {
            if buffer.len() == MAX_RECORD_BYTES {
                return Err(io::Error::other(
                    "The local ownership account record exceeds 256 KiB.",
                ));
            }
            let next = (buffer.len() * 2).min(MAX_RECORD_BYTES);
            buffer.as_mut_slice().zeroize();
            buffer.resize(next, 0);
            continue;
        }
        if status != 0 {
            let error = io::Error::from_raw_os_error(status);
            return Err(io::Error::new(
                error.kind(),
                format!("Could not look up the local ownership account ({error})."),
            ));
        }
        if result.is_null() {
            return Err(io::Error::new(io::ErrorKind::NotFound, not_found));
        }
        if result != record.as_mut_ptr() {
            return Err(invalid_record());
        }
        // SAFETY: A successful lookup with result equal to the supplied
        // record pointer has initialized the record.
        return accept(&buffer, unsafe { record.assume_init_ref() });
    }
}

fn display_label(
    buffer: &[u8],
    returned: *const c_char,
    actual: u32,
    requested: u32,
) -> io::Result<String> {
    let name = returned
        .addr()
        .checked_sub(buffer.as_ptr().addr())
        .and_then(|offset| buffer.get(offset..))
        .and_then(|suffix| CStr::from_bytes_until_nul(suffix).ok())
        .map(CStr::to_bytes)
        .filter(|name| (1..=MAX_NAME_BYTES).contains(&name.len()) && actual == requested)
        .ok_or_else(invalid_record)?;
    Ok(String::from_utf8_lossy(name).into_owned())
}

fn exact_name(buffer: &[u8], returned: *const c_char, expected: &CStr) -> bool {
    returned
        .addr()
        .checked_sub(buffer.as_ptr().addr())
        .and_then(|offset| buffer.get(offset..))
        .is_some_and(|name| name.starts_with(expected.to_bytes_with_nul()))
}

fn invalid_record() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The local account lookup did not return the exact name with a valid ownership ID.",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ptr;

    use super::*;

    struct Record {
        name: *const c_char,
        id: u32,
    }

    unsafe extern "C" fn fixture(
        name: *const c_char,
        record: *mut Record,
        buffer: *mut c_char,
        capacity: usize,
        result: *mut *mut Record,
    ) -> c_int {
        // SAFETY: lookup passes a live NUL-terminated input name.
        let name = unsafe { CStr::from_ptr(name) };
        match name.to_bytes() {
            b"oversized" => return libc::ERANGE,
            b"grows" if capacity == INITIAL_RECORD_BYTES => return libc::ERANGE,
            b"missing" => return 0,
            _ => {}
        }
        let returned_name = if name.to_bytes() == b"alias" {
            c"other"
        } else {
            name
        };
        let bytes = returned_name.to_bytes_with_nul();
        if bytes.len() > capacity {
            return libc::ERANGE;
        }
        let id = if name.to_bytes() == b"reserved" {
            u32::MAX
        } else {
            42
        };
        // SAFETY: lookup supplies independent writable allocations for each
        // output. The returned name fits in the provided buffer.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast(), bytes.len());
            record.write(Record { name: buffer, id });
            result.write(record);
        }
        0
    }

    fn resolve_fixture(name: &CStr, guard: &impl Fn() -> io::Result<()>) -> io::Result<u32> {
        lookup(name, fixture, |record| (record.name, record.id), guard)
    }

    #[test]
    fn local_ownership_principals_bound_records_and_require_exact_names() {
        assert_eq!(resolve_fixture(c"grows", &|| Ok(())).unwrap(), 42);
        assert!(
            resolve_fixture(c"oversized", &|| Ok(()))
                .unwrap_err()
                .to_string()
                .contains("256 KiB")
        );
        assert!(
            resolve_fixture(c"missing", &|| Ok(()))
                .unwrap_err()
                .to_string()
                .contains("not found")
        );
        for name in [c"alias", c"reserved"] {
            assert!(
                resolve_fixture(name, &|| Ok(()))
                    .unwrap_err()
                    .to_string()
                    .contains("valid ownership ID")
            );
        }
    }

    #[test]
    fn local_ownership_principals_discard_lookup_when_the_guard_changes() {
        let calls = Cell::new(0);
        let result = resolve_fixture(c"exact", &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Ok(())
            } else {
                Err(io::Error::other("Fixture cancelled"))
            }
        });
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Fixture cancelled")
        );
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn local_ownership_principals_resolve_native_user_and_group_records() {
        #[cfg(target_os = "macos")]
        const GROUP: &str = "wheel";
        #[cfg(target_os = "linux")]
        const GROUP: &str = "root";
        assert_eq!(resolve(Kind::User, "root", &|| Ok(())).unwrap(), 0);
        assert_eq!(resolve(Kind::Group, GROUP, &|| Ok(())).unwrap(), 0);
        assert_eq!(display_name(Kind::User, 0, &|| Ok(())).unwrap(), "root");
        assert_eq!(display_name(Kind::Group, 0, &|| Ok(())).unwrap(), GROUP);
    }
}
