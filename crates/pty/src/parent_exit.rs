use std::fs::File;
use std::io::{self, PipeWriter, Read};
use std::process::{Command, Stdio};

#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;

#[cfg(target_os = "macos")]
pub(crate) const EXPECTED_PARENT: &str = "KORTTY_WORKER_PARENT_PID";

#[cfg(test)]
#[path = "parent_exit/tests.rs"]
mod tests;

/// The parent-owned end of a worker's lifetime pipe.
#[must_use = "keep the guard in the parent while the child may run"]
pub struct ParentGuard {
    _writer: PipeWriter,
}

/// A worker's read end of the lifetime pipe attached by its parent.
pub struct ParentExit {
    pipe: File,
    #[cfg(target_os = "macos")]
    queue: OwnedFd,
}

impl ParentExit {
    /// Assigns the read end to the child's stderr. The child must reserve that
    /// standard handle for this pipe and send diagnostics through another path.
    /// Keep the returned guard in the parent for as long as the child may run.
    pub fn attach(command: &mut Command) -> io::Result<ParentGuard> {
        let (reader, writer) = io::pipe()?;
        command.stderr(Stdio::from(reader));
        #[cfg(target_os = "macos")]
        command.env(EXPECTED_PARENT, std::process::id().to_string());
        Ok(ParentGuard { _writer: writer })
    }

    /// Opens the lifetime pipe in a child, rejecting absent, invalid or already
    /// closed pipe handles before the worker starts.
    pub fn watch() -> io::Result<Self> {
        let pipe = inherited_pipe()?;
        #[cfg(target_os = "macos")]
        let queue = macos::watch(&pipe)?;
        Ok(Self {
            pipe,
            #[cfg(target_os = "macos")]
            queue,
        })
    }

    /// Waits for the parent to close the guard or exit. This does not read the
    /// worker's input stream and is intended to run on a dedicated thread.
    pub fn wait(mut self) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if macos::parent_exited(&self.queue)? {
            return Ok(());
        }
        match self.pipe.read_exact(&mut [0]) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
            Ok(()) => Err(invalid_pipe()),
        }
    }
}

fn invalid_pipe() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid parent lifetime pipe")
}

#[cfg(target_os = "macos")]
#[path = "parent_exit/macos.rs"]
mod macos;

#[cfg(unix)]
fn inherited_pipe() -> io::Result<File> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::FileTypeExt as _;

    // SAFETY: this duplicates a numeric descriptor as close-on-exec, above stdio.
    let descriptor = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new descriptor owned by this call.
    let file = unsafe { File::from_raw_fd(descriptor) };
    // SAFETY: F_GETFL reads flags on the live duplicate.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY || !file.metadata()?.file_type().is_fifo() {
        return Err(invalid_pipe());
    }
    let mut event = libc::pollfd {
        fd: descriptor,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: poll inspects one live descriptor and writes into its event record.
        match unsafe { libc::poll(&mut event, 1, 0) } {
            0 => return Ok(file),
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            _ if event.revents & libc::POLLHUP != 0 => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "parent already exited",
                ));
            }
            _ => return Err(invalid_pipe()),
        }
    }
}

#[cfg(windows)]
fn inherited_pipe() -> io::Result<File> {
    use std::os::windows::io::FromRawHandle as _;
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{
        DUPLICATE_SAME_ACCESS, DuplicateHandle, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: GetStdHandle only reads the current process's standard-handle table.
    let inherited = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if inherited == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    if inherited.is_null() {
        return Err(invalid_pipe());
    }
    let mut duplicate = null_mut();
    // SAFETY: both pseudo-handles name this process. The output pointer is writable.
    let ok = unsafe {
        let process = GetCurrentProcess();
        DuplicateHandle(
            process,
            inherited,
            process,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle succeeded and returned a new owned handle.
    let file = unsafe { File::from_raw_handle(duplicate) };
    let mut available = 0;
    // SAFETY: the handle remains live. PeekNamedPipe checks that it is a readable
    // pipe. The only output pointer receives the byte count. No other thread
    // reads this handle during the startup check.
    let ok = unsafe {
        PeekNamedPipe(
            duplicate,
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if available != 0 {
        return Err(invalid_pipe());
    }
    Ok(file)
}
