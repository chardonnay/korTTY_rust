use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::Command;

/// Sets a child process's maximum writable file size, without raising inherited limits.
pub fn limit_child_file_size(command: &mut Command, bytes: u32) -> io::Result<()> {
    let mut inherited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `inherited` is initialized, writable, and valid for the system call.
    if unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut inherited) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes = inherited.rlim_cur.min(bytes.into());
    let limit = libc::rlimit {
        rlim_cur: bytes,
        rlim_max: bytes,
    };
    // SAFETY: The callback only calls async-signal-safe setrlimit and returns the raw OS error.
    unsafe {
        command.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}
