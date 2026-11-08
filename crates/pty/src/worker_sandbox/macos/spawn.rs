//! Darwin's exec descriptor allowlist avoids a fork-time inheritance race.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;

pub(super) fn launch(
    executable: &Path,
    argument: &str,
    transport: RawFd,
    sources: [RawFd; 4],
    env: &[CString],
) -> io::Result<libc::pid_t> {
    let path = CString::new(executable.as_os_str().as_bytes())?;
    let argument = CString::new(argument)?;
    let descriptor = CString::new(transport.to_string())?;
    let argv = pointers(
        [&path, &argument, &descriptor]
            .into_iter()
            .map(CString::as_c_str),
    );
    let envp = pointers(env.iter().map(CString::as_c_str));
    let attributes = Attributes::new()?;
    let mut actions = Actions::new()?;
    let sources = sources
        .into_iter()
        .map(|fd| duplicate(fd, transport + 1))
        .collect::<io::Result<Vec<_>>>()?;
    for (source, target) in sources.iter().zip([
        libc::STDIN_FILENO,
        libc::STDOUT_FILENO,
        libc::STDERR_FILENO,
        transport,
    ]) {
        actions.duplicate(source.as_raw_fd(), target)?;
    }
    let mut pid = 0;
    // SAFETY: the path, terminated pointer arrays, opaque spawn objects and
    // descriptor owners remain live until this synchronous call returns.
    status(unsafe {
        libc::posix_spawn(
            &mut pid,
            path.as_ptr(),
            &actions.0,
            &attributes.0,
            argv.as_ptr(),
            envp.as_ptr(),
        )
    })?;
    Ok(pid)
}

fn pointers<'a>(strings: impl Iterator<Item = &'a CStr>) -> Vec<*mut libc::c_char> {
    strings
        .map(|value| value.as_ptr().cast_mut())
        .chain(std::iter::once(null_mut()))
        .collect()
}

fn duplicate(source: RawFd, first: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: the caller owns the source. The duplicate is never inheritable
    // in the desktop and cannot overlap any later file-action destination.
    let fd = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, first) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new descriptor owned by this function.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

struct Attributes(libc::posix_spawnattr_t);

impl Attributes {
    fn new() -> io::Result<Self> {
        let mut raw = null_mut();
        // SAFETY: the output pointer is writable and initialized on success.
        status(unsafe { libc::posix_spawnattr_init(&mut raw) })?;
        let mut attributes = Self(raw);
        // SAFETY: the initialized spawn attributes are live and exclusively borrowed.
        status(unsafe {
            libc::posix_spawnattr_setflags(
                &mut attributes.0,
                (libc::POSIX_SPAWN_CLOEXEC_DEFAULT | libc::POSIX_SPAWN_START_SUSPENDED)
                    as libc::c_short,
            )
        })?;
        Ok(attributes)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: this guard owns one successfully initialized attribute list.
        unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
    }
}

struct Actions(libc::posix_spawn_file_actions_t);

impl Actions {
    fn new() -> io::Result<Self> {
        let mut raw = null_mut();
        // SAFETY: the output pointer is writable and initialized on success.
        status(unsafe { libc::posix_spawn_file_actions_init(&mut raw) })?;
        Ok(Self(raw))
    }

    fn duplicate(&mut self, source: RawFd, destination: RawFd) -> io::Result<()> {
        // SAFETY: the action list is live. The descriptor owners outlive launch.
        status(unsafe { libc::posix_spawn_file_actions_adddup2(&mut self.0, source, destination) })
    }
}

impl Drop for Actions {
    fn drop(&mut self) {
        // SAFETY: this guard owns one successfully initialized action list.
        unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
    }
}

fn status(code: libc::c_int) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code))
    }
}
