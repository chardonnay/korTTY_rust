use std::ffi::{c_int, c_uint, c_void};
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd as _;
use std::ptr::null_mut;

const ACL_TYPE_EXTENDED: c_uint = 0x100;

unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, acl_type: c_uint) -> *mut c_void;
    fn acl_free(acl: *mut c_void) -> c_int;
    fn acl_valid(acl: *mut c_void) -> c_int;
    fn acl_get_entry(acl: *mut c_void, index: c_int, entry: *mut *mut c_void) -> c_int;
    fn acl_get_tag_type(entry: *mut c_void, kind: *mut c_uint) -> c_int;
    fn acl_set_fd_np(fd: c_int, acl: *mut c_void, acl_type: c_uint) -> c_int;
    fn acl_size(acl: *mut c_void) -> isize;
    fn acl_copy_ext(buffer: *mut c_void, acl: *mut c_void, size: isize) -> isize;
    fn acl_copy_int(buffer: *const c_void) -> *mut c_void;
}

/// An owned, bounded copy of a Darwin extended ACL, including inherited flags.
#[derive(Clone, Eq, PartialEq)]
pub struct ExtendedAcl(Option<Vec<u64>>);

impl ExtendedAcl {
    pub fn capture(file: &File) -> io::Result<Self> {
        // SAFETY: The descriptor stays live throughout this bounded ACL query.
        let raw = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            return if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ) {
                Ok(Self(None))
            } else {
                Err(error)
            };
        }
        let acl = OwnedAcl(raw);
        // SAFETY: The guard owns this live native ACL.
        let size = unsafe { acl_size(acl.0) };
        if !(1..=64 * 1024).contains(&size) {
            return Err(io::Error::other("Darwin ACL exceeds its size limit"));
        }
        let mut bytes = vec![0_u64; (size as usize).div_ceil(size_of::<u64>())];
        // SAFETY: The aligned buffer covers the size obtained from the same ACL.
        if unsafe { acl_copy_ext(bytes.as_mut_ptr().cast(), acl.0, size) } != size {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(Some(bytes)))
    }

    pub fn apply_to(&self, file: &File) -> io::Result<()> {
        if let Some(bytes) = &self.0 {
            // SAFETY: These aligned bytes originate only from our successful,
            // bounded acl_copy_ext call and have not been modified or persisted.
            let raw = unsafe { acl_copy_int(bytes.as_ptr().cast()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let acl = OwnedAcl(raw);
            // SAFETY: Both the file descriptor and owned ACL remain live.
            if unsafe { acl_set_fd_np(file.as_raw_fd(), acl.0, ACL_TYPE_EXTENDED) } != 0 {
                return Err(io::Error::last_os_error());
            }
        } else if Self::capture(file)?.0.is_some() {
            // SAFETY: Apple Libc explicitly supports _FILESEC_REMOVE_ACL ((void*)1)
            // in acl_copy_ext and acl_set_fd_np; this sentinel is never dereferenced.
            // The descriptor refers to the caller's held, newly created object.
            if unsafe {
                acl_set_fd_np(
                    file.as_raw_fd(),
                    std::ptr::without_provenance_mut(1),
                    ACL_TYPE_EXTENDED,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        self.require_applied(file)
    }

    pub fn require_applied(&self, file: &File) -> io::Result<()> {
        if Self::capture(file)? == *self {
            Ok(())
        } else {
            Err(io::Error::other("Darwin file access ACL changed"))
        }
    }
}

struct OwnedAcl(*mut c_void);

impl Drop for OwnedAcl {
    fn drop(&mut self) {
        // SAFETY: This guard exclusively owns one native ACL allocation.
        unsafe { acl_free(self.0) };
    }
}

/// Removes inherited ACL entries before private staging receives any user bytes.
pub fn clear_extended_acl(file: &File) -> io::Result<()> {
    ExtendedAcl(None).apply_to(file)
}

/// Allows absent or deny-only ACLs. These cannot grant access beyond POSIX ownership and mode.
pub fn require_no_granting_extended_acl(file: &File) -> io::Result<()> {
    const EINVAL: i32 = 22;
    const ACL_EXTENDED_DENY: c_uint = 2;
    const ACL_MAX_ENTRIES: c_int = 128;

    struct Acl(*mut c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: The guard owns one ACL allocation, and frees it once.
            unsafe { acl_free(self.0) };
        }
    }

    // SAFETY: The file keeps the descriptor live for the ACL lookup.
    let raw = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        };
    }
    let acl = Acl(raw);
    // SAFETY: The ACL is live and owned by the guard.
    if unsafe { acl_valid(acl.0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for index in 0..=ACL_MAX_ENTRIES {
        let mut entry = null_mut();
        // SAFETY: The validated ACL and the output slot are live.
        if unsafe { acl_get_entry(acl.0, index, &mut entry) } != 0 {
            let error = io::Error::last_os_error();
            // With a validated ACL and a nonnegative index, EINVAL denotes its end.
            return if error.raw_os_error() == Some(EINVAL) {
                Ok(())
            } else {
                Err(error)
            };
        }
        let mut kind = 0;
        // SAFETY: A successful lookup returned an entry borrowed from the live ACL.
        if unsafe { acl_get_tag_type(entry, &mut kind) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if kind != ACL_EXTENDED_DENY {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "An external model path grants access through a macOS extended ACL",
            ));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "macOS ACL entry limit exceeded",
    ))
}

/// Requires a descriptor with no Darwin extended ACL in addition to its POSIX mode.
pub fn require_no_extended_acl(file: &File) -> io::Result<()> {
    // SAFETY: the file keeps the descriptor live for this lookup.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // Darwin reports ENOENT when the descriptor has no FILESEC_ACL property.
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        };
    }
    // SAFETY: a non-null result is a fresh ACL allocation owned by this call.
    unsafe { acl_free(acl) };
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "The logfile staging directory inherited an extended ACL",
    ))
}
