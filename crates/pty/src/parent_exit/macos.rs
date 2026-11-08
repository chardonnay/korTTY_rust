use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::ptr::{null, null_mut};

use super::{EXPECTED_PARENT, invalid_pipe};

pub(super) fn watch(pipe: &File) -> io::Result<OwnedFd> {
    let parent: libc::pid_t = std::env::var(EXPECTED_PARENT)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|&pid| pid > 0)
        .ok_or_else(invalid_pipe)?;
    verify_parent(parent)?;
    // Darwin creates pipes with pipe() then sets close-on-exec. Observe the
    // process directly as well, so a concurrent fork that inherited the pipe's
    // writer cannot keep a worker alive after its real parent exits.
    // SAFETY: kqueue takes no arguments and returns a new descriptor on success.
    let descriptor = unsafe { libc::kqueue() };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: kqueue returned one owned descriptor.
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    // SAFETY: F_SETFD sets close-on-exec on the live descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let changes = [
        registration(parent as usize, libc::EVFILT_PROC, libc::NOTE_EXIT),
        registration(pipe.as_raw_fd() as usize, libc::EVFILT_READ, 0),
    ];
    // SAFETY: the queue, watched pipe and initialized change array remain live.
    if unsafe { libc::kevent(descriptor, changes.as_ptr(), 2, null_mut(), 0, null()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // An exited process may have its PID reused between our first check and registration.
    verify_parent(parent)?;
    Ok(queue)
}

fn verify_parent(expected: libc::pid_t) -> io::Result<()> {
    // SAFETY: getppid has no arguments and reads only the caller's parent identity.
    if unsafe { libc::getppid() } == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "parent already exited",
        ))
    }
}

fn registration(ident: usize, filter: i16, fflags: u32) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags,
        data: 0,
        udata: null_mut(),
    }
}

/// Returns true on a parent process exit, or false if the pipe can be read.
pub(super) fn parent_exited(queue: &OwnedFd) -> io::Result<bool> {
    loop {
        let mut event = MaybeUninit::<libc::kevent>::uninit();
        // SAFETY: the descriptor is live and the output holds one event.
        match unsafe { libc::kevent(queue.as_raw_fd(), null(), 0, event.as_mut_ptr(), 1, null()) } {
            1 => {
                // SAFETY: kevent wrote the returned event.
                let event = unsafe { event.assume_init() };
                return match event.filter {
                    libc::EVFILT_PROC if event.fflags & libc::NOTE_EXIT != 0 => Ok(true),
                    libc::EVFILT_READ if event.flags & libc::EV_ERROR == 0 => Ok(false),
                    _ => Err(invalid_pipe()),
                };
            }
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            _ => return Err(invalid_pipe()),
        }
    }
}
