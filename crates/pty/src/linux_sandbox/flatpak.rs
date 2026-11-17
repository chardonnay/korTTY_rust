//! System paths supplied by the installed Flatpak mount namespace.

use std::fs::{File, Metadata};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use super::{probe_path, system_result};

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

// Linux uapi/linux/openat2.h.
const RESOLVE_NO_SYMLINKS: u64 = 0x04;

pub(super) fn installed_namespace() -> io::Result<bool> {
    if !crate::flatpak::running() {
        return Ok(false);
    }
    let marker = path_handle(Path::new("/.flatpak-info"))?;
    if !marker.metadata()?.is_file() || !read_only(&marker)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the Flatpak instance metadata must be read-only",
        ));
    }
    Ok(true)
}

pub(super) fn require_system_path(
    requested: &Path,
    resolved: &Path,
    handle: &File,
    metadata: &Metadata,
) -> io::Result<()> {
    // Flatpak reserves /usr, /etc and /dev. It mounts the runtime and system
    // configuration read-only, even when files belong to the installing user.
    let device = match requested.to_str() {
        Some("/dev/null") => Some((1, 3)),
        Some("/dev/zero") => Some((1, 5)),
        Some("/dev/urandom") => Some((1, 9)),
        Some("/dev/tty") => Some((5, 0)),
        _ => None,
    };
    let trusted = if let Some((major, minor)) = device {
        requested == resolved
            && metadata.file_type().is_char_device()
            && metadata.rdev() == libc::makedev(major, minor)
    } else if requested.starts_with("/etc") {
        metadata.is_file() && runtime_configuration(requested, resolved) && read_only(handle)?
    } else {
        metadata.is_dir()
            && resolved.starts_with("/usr")
            && resolved != Path::new("/usr")
            && read_only(handle)?
    };
    if !trusted {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "untrusted Flatpak sandbox system path ({})",
                resolved.display()
            ),
        ));
    }
    Ok(())
}

fn runtime_configuration(requested: &Path, resolved: &Path) -> bool {
    requested == resolved
        || resolved.starts_with("/usr/etc") && resolved.file_name() == requested.file_name()
        || matches!(requested.to_str(), Some("/etc/hosts" | "/etc/resolv.conf"))
            && resolved.parent() == Some(Path::new("/run/host/monitor"))
            && resolved.file_name() == requested.file_name()
        || requested == Path::new("/etc/localtime") && resolved.starts_with("/usr/share/zoneinfo")
}

/// Opens the canonical name without following any symlink. Files that
/// Bubblewrap mounts from anonymous descriptors can appear as deleted in procfs.
pub(super) fn path_handle(path: &Path) -> io::Result<File> {
    let name = probe_path(path)?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: the terminated path and the kernel's open_how layout stay live
    // throughout the call. This operation only opens an existing path.
    let fd = system_result(unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            name.as_ptr(),
            &raw const how,
            size_of::<OpenHow>(),
        )
    })?;
    // SAFETY: openat2 returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd as libc::c_int) })
}

fn read_only(handle: &File) -> io::Result<bool> {
    let mut status = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the descriptor is live and the statvfs output buffer is writable.
    if unsafe { libc::fstatvfs(handle.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful fstatvfs initialized the output buffer.
    let flags = unsafe { status.assume_init() }.f_flag;
    Ok(flags & libc::ST_RDONLY != 0)
}
