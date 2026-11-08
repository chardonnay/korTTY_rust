use std::ffi::{c_int, c_uint, c_void};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd as _;

const ACL_TYPE_EXTENDED: c_uint = 0x100;

unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, acl_type: c_uint) -> *mut c_void;
    fn acl_free(acl: *mut c_void) -> c_int;
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
