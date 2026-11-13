use std::env;
use std::ffi::{CStr, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use crate::shell_integration::unix::{Startup, default_snippet};
use crate::{JobProcessIdentity, LaunchOptions, Size, closed};

#[path = "unix/job_processes.rs"]
mod job_processes;
#[cfg(target_os = "macos")]
#[path = "unix/macos_sandbox.rs"]
mod macos_sandbox;
#[path = "unix/shell_directory.rs"]
mod shell_directory;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64",
    target_endian = "little",
))]
use crate::linux_sandbox;

const HANGUP_GRACE: Duration = Duration::from_millis(150);
const SHUTDOWN_POLL: Duration = Duration::from_millis(10);

type SharedFile = Arc<Mutex<Option<File>>>;

pub(crate) struct Control {
    child: Child,
    executable: Option<shell_directory::Executable>,
    master: Option<Arc<File>>,
    wake: Option<UnixStream>,
    output: SharedFile,
    input: SharedFile,
    startup: Option<Startup>,
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_pointer_width = "64",
        target_endian = "little",
    ))]
    sandbox_directory: Option<linux_sandbox::SessionDirectory>,
    #[cfg(target_os = "macos")]
    sandbox_directory: Option<Box<macos_sandbox::SessionDirectory>>,
}

#[derive(Clone)]
pub(crate) struct JobProcessProbe {
    master: Weak<File>,
    shell: u32,
}

impl JobProcessProbe {
    pub(crate) fn find<T>(
        &self,
        previous: Option<JobProcessIdentity>,
        inspect: impl Fn(&str, &[&str]) -> Option<T>,
    ) -> Option<(JobProcessIdentity, T)> {
        let master = self.master.upgrade()?;
        let running = || matches!(child_pid_running(self.shell), Ok(true));
        if !running() {
            return None;
        }
        let found = job_processes::find(&master, self.shell, previous, inspect)?;
        running().then_some(found)
    }
}

impl Control {
    pub(crate) fn sandbox_backend(&self) -> Option<&'static str> {
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_pointer_width = "64",
            target_endian = "little",
        ))]
        {
            self.sandbox_directory.as_ref().map(|_| "landlock")
        }
        #[cfg(target_os = "macos")]
        {
            self.sandbox_directory
                .as_deref()
                .map(macos_sandbox::SessionDirectory::backend)
        }
        #[cfg(not(any(
            target_os = "macos",
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64"),
                target_pointer_width = "64",
                target_endian = "little",
            ),
        )))]
        None
    }

    pub(crate) fn shell_pid(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn job_process_probe(&self) -> Option<JobProcessProbe> {
        Some(JobProcessProbe {
            master: Arc::downgrade(self.master.as_ref()?),
            shell: self.child.id(),
        })
    }

    pub(crate) fn shell_directory(&self) -> io::Result<PathBuf> {
        let executable = self.executable.ok_or_else(shell_directory::unverified)?;
        let master = self.master.as_ref().ok_or_else(closed)?;
        shell_directory::resolve(&self.child, master, executable)
    }

    pub(crate) fn spawn(
        size: Size,
        term: &str,
        options: &LaunchOptions,
    ) -> io::Result<(Self, PtyReader, PtyWriter)> {
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_pointer_width = "64",
            target_endian = "little",
        ))]
        let (restrictions, sandbox_directory) = match &options.sandbox {
            Some(sandbox) => {
                let (restrictions, directory) = linux_sandbox::prepare(
                    options.working_directory.as_deref().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "sandbox workspace required")
                    })?,
                    &sandbox.profile_root,
                )?;
                (Some(restrictions), Some(directory))
            }
            None => (None, None),
        };
        #[cfg(target_os = "macos")]
        let (restrictions, sandbox_directory) = match &options.sandbox {
            Some(sandbox) => {
                let workspace = options.working_directory.as_deref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "sandbox workspace required")
                })?;
                let (restrictions, directory) =
                    macos_sandbox::prepare(workspace, &sandbox.profile_root)?;
                (Some(Arc::new(restrictions)), Some(directory))
            }
            None => (None, None),
        };
        let master = open_master()?;
        let master_fd = master.as_raw_fd();
        // SAFETY: the master remains open throughout both POSIX PTY calls.
        os_result(unsafe { libc::grantpt(master_fd) })?;
        // SAFETY: the granted master remains open while unlocking its slave.
        os_result(unsafe { libc::unlockpt(master_fd) })?;
        let slave_name = slave_path(master_fd)?;
        let slave = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(&slave_name)?;
        set_size(master_fd, size, 0, 0)?;

        let (wake, signal) = UnixStream::pair()?;
        let read_signal = signal.try_clone()?;
        let output = Arc::new(Mutex::new(Some(master.try_clone()?)));
        let input = Arc::new(Mutex::new(Some(master.try_clone()?)));
        let (program, arguments) = match &options.command {
            Some(words) => (OsString::from(&words[0]), &words[1..]),
            None => (default_shell(), &[][..]),
        };
        let executable = shell_directory::Executable::read(Path::new(&program)).ok();
        #[cfg(target_os = "macos")]
        let (mut command, attestation) = match restrictions.as_ref().zip(sandbox_directory.as_ref())
        {
            Some((restrictions, directory)) => {
                let (command, ready) =
                    restrictions.command(directory, &program, Path::new(&slave_name))?;
                (command, Some(ready))
            }
            None => (Command::new(program), None),
        };
        #[cfg(not(target_os = "macos"))]
        let mut command = Command::new(program);
        let startup = if options.shell_integration && options.sandbox.is_none() {
            let snippet = default_snippet(command.get_program()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the selected local shell does not support automatic integration",
                )
            })?;
            Some(Startup::prepare(&mut command, snippet, arguments)?)
        } else {
            command.args(arguments);
            None
        };
        if options.sandbox.is_none()
            && let Some(directory) = &options.working_directory
        {
            command.current_dir(directory);
        }
        command
            .env("TERM", term)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?));
        #[cfg(target_os = "macos")]
        if attestation.is_none() {
            command.stderr(Stdio::from(slave));
        }
        #[cfg(not(target_os = "macos"))]
        command.stderr(Stdio::from(slave));
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_pointer_width = "64",
            target_endian = "little",
        ))]
        if let Some(directory) = &sandbox_directory {
            command
                .env_clear()
                .env("TERM", term)
                .env("PATH", "/usr/bin:/bin")
                .env("LANG", "C.UTF-8")
                .env("HOME", directory.path())
                .env("TMPDIR", directory.path())
                .env("HISTFILE", directory.path().join("history"))
                .env("KORTTY_SANDBOX", "landlock");
        }
        #[cfg(target_os = "macos")]
        let child_restrictions = restrictions.clone();
        // SAFETY: the closure runs after fork and uses only async-signal-safe
        // system calls. std sets up the slave on fd 0 before running it.
        unsafe {
            command.pre_exec(move || {
                os_result(libc::setsid())?;
                #[cfg(target_os = "macos")]
                let request = libc::c_ulong::from(libc::TIOCSCTTY);
                #[cfg(target_os = "linux")]
                let request = libc::TIOCSCTTY;
                os_result(libc::ioctl(libc::STDIN_FILENO, request, 0))?;
                #[cfg(all(
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64"),
                    target_pointer_width = "64",
                    target_endian = "little",
                ))]
                if let Some(restrictions) = &restrictions {
                    restrictions.apply()?;
                }
                #[cfg(target_os = "macos")]
                if let Some(restrictions) = &child_restrictions {
                    restrictions.apply()?;
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(command);
        #[cfg(target_os = "macos")]
        let child = {
            let mut child = child;
            if let Some((restrictions, ready)) = restrictions.as_ref().zip(attestation)
                && let Err(error) = restrictions.attest(ready)
            {
                let _ = signal_group(-(child.id() as libc::pid_t), libc::SIGKILL);
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            child
        };

        let reader = PtyReader {
            file: Arc::clone(&output),
            signal: read_signal,
        };
        let writer = PtyWriter {
            file: Arc::clone(&input),
            signal,
        };
        Ok((
            Self {
                child,
                executable,
                master: Some(Arc::new(master)),
                wake: Some(wake),
                output,
                input,
                startup,
                #[cfg(all(
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64"),
                    target_pointer_width = "64",
                    target_endian = "little",
                ))]
                sandbox_directory,
                #[cfg(target_os = "macos")]
                sandbox_directory: sandbox_directory.map(Box::new),
            },
            reader,
            writer,
        ))
    }

    pub(crate) fn resize(&mut self, size: Size, width: u16, height: u16) -> io::Result<()> {
        set_size(
            self.master.as_ref().ok_or_else(closed)?.as_raw_fd(),
            size,
            width,
            height,
        )
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.startup.take();
        }
        Ok(status)
    }

    pub(crate) fn shutdown(&mut self) -> io::Result<()> {
        if let Some(master) = self.master.take() {
            // Closing this socket wakes both pollers on the other endpoint.
            self.wake.take();
            self.output
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take();
            self.input
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .take();
            drop(master);
        }
        self.reap()?;
        self.startup.take();
        #[cfg(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_pointer_width = "64",
            target_endian = "little",
        ))]
        self.sandbox_directory.take();
        #[cfg(target_os = "macos")]
        self.sandbox_directory.take();
        Ok(())
    }

    fn reap(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }

        // The shell became a session and process-group leader in pre_exec.
        // Until we reap it, its process ID cannot be reused by another group.
        let group = -(self.child.id() as libc::pid_t);
        signal_group(group, libc::SIGHUP)?;
        let deadline = Instant::now() + HANGUP_GRACE;
        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(SHUTDOWN_POLL);
        }
        signal_group(group, libc::SIGKILL)?;
        self.child.wait()?;
        Ok(())
    }
}

/// Reads output bytes from the local shell. Shutdown interrupts a blocked read.
pub struct PtyReader {
    file: SharedFile,
    signal: UnixStream,
}

impl Read for PtyReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let file = self.file.lock().unwrap_or_else(|err| err.into_inner());
        let Some(mut file) = file.as_ref() else {
            return Ok(0);
        };
        while ready(file.as_raw_fd(), self.signal.as_raw_fd(), libc::POLLIN)? {
            match file.read(buffer) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                // Linux reports EIO on the master after the last slave closes.
                Err(err) if err.raw_os_error() == Some(libc::EIO) => return Ok(0),
                result => return result,
            }
        }
        Ok(0)
    }
}

/// Writes input bytes to the local shell. Shutdown interrupts a blocked write.
pub struct PtyWriter {
    file: SharedFile,
    signal: UnixStream,
}

impl Write for PtyWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let file = self.file.lock().unwrap_or_else(|err| err.into_inner());
        let mut file = file.as_ref().ok_or_else(closed)?;
        while ready(file.as_raw_fd(), self.signal.as_raw_fd(), libc::POLLOUT)? {
            match file.write(buffer) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(err) if err.raw_os_error() == Some(libc::EIO) => return Err(closed()),
                result => return result,
            }
        }
        Err(closed())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn default_shell() -> OsString {
    if let Some(shell) = env::var_os("SHELL").filter(|shell| Path::new(shell).is_absolute()) {
        return shell;
    }
    if Path::new("/bin/zsh").is_file() {
        return "/bin/zsh".into();
    }
    if Path::new("/bin/bash").is_file() {
        return "/bin/bash".into();
    }
    "/bin/sh".into()
}

fn open_master() -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK);

    #[cfg(target_os = "macos")]
    {
        // XNU can give concurrent opens the same PTY clone slot. Serialize the
        // allocation only; shell startup and I/O remain independent.
        static ALLOCATION: Mutex<()> = Mutex::new(());
        const EREDRIVEOPEN: i32 = -6; // Private Darwin errno for retrying open.
        let open = || {
            let _guard = ALLOCATION.lock().unwrap_or_else(|error| error.into_inner());
            options.open("/dev/ptmx")
        };
        for delay in [Duration::from_millis(1), Duration::from_millis(2)] {
            match open() {
                Err(error) if error.raw_os_error() == Some(EREDRIVEOPEN) => {
                    // An allocation by another process can still race us.
                    thread::sleep(delay);
                }
                result => return result,
            }
        }
        open()
    }
    #[cfg(not(target_os = "macos"))]
    options.open("/dev/ptmx")
}

fn slave_path(master: RawFd) -> io::Result<OsString> {
    let mut path = [0_u8; 128];
    #[cfg(target_os = "macos")]
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes to the supplied buffer.
    os_result(unsafe { libc::ioctl(master, libc::TIOCPTYGNAME.into(), path.as_mut_ptr()) })?;
    #[cfg(target_os = "linux")]
    {
        // SAFETY: ptsname_r receives the buffer's writable pointer and length.
        let result = unsafe { libc::ptsname_r(master, path.as_mut_ptr().cast(), path.len()) };
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result));
        }
    }
    let name = CStr::from_bytes_until_nul(&path).map_err(io::Error::other)?;
    Ok(OsStr::from_bytes(name.to_bytes()).to_owned())
}

fn set_size(fd: RawFd, size: Size, width: u16, height: u16) -> io::Result<()> {
    let dimensions = libc::winsize {
        ws_row: size.rows,
        ws_col: size.columns,
        ws_xpixel: width,
        ws_ypixel: height,
    };
    // SAFETY: fd is a live PTY and dimensions has the TIOCSWINSZ ABI.
    os_result(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &dimensions) })
}

fn ready(fd: RawFd, signal: RawFd, events: libc::c_short) -> io::Result<bool> {
    let mut watched = [
        libc::pollfd {
            fd: signal,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd,
            events,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: the slice holds both descriptors for the duration of poll.
        let status = unsafe { libc::poll(watched.as_mut_ptr(), watched.len() as libc::nfds_t, -1) };
        match os_result(status) {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
            Ok(()) => return Ok(watched[0].revents == 0),
        }
    }
}

fn signal_group(group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: the caller owns the live session leader's process group.
    let result = match os_result(unsafe { libc::kill(group, signal) }) {
        // Darwin can deny group delivery while still allowing the session
        // leader to be signaled directly, including during tty teardown.
        Err(err) if err.raw_os_error() == Some(libc::EPERM) => {
            // SAFETY: the process is the same child and has not been reaped.
            os_result(unsafe { libc::kill(-group, signal) })
        }
        result => result,
    };
    match result {
        Err(err) if err.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        result => result,
    }
}

fn os_result(status: libc::c_int) -> io::Result<()> {
    if status == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn child_running(child: &Child) -> io::Result<bool> {
    child_pid_running(child.id())
}

fn child_pid_running(pid: u32) -> io::Result<bool> {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: the child is owned by the caller and info is writable. WNOWAIT
        // preserves the child's status and PID until the owner explicitly reaps it.
        let status = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        match os_result(status) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
            Ok(()) => {
                // SAFETY: the record is zero-initialized and waitid fills it on exit.
                return Ok(unsafe { info.assume_init() }.si_signo == 0);
            }
        }
    }
}

/// Sets limits in the disposable PDF parser before it accepts any document bytes.
pub fn limit_pdf_worker() -> io::Result<()> {
    let limits: [(_, libc::rlim_t); 3] = [
        (libc::RLIMIT_AS, 1024 * 1024 * 1024),
        (libc::RLIMIT_CPU, 6),
        (libc::RLIMIT_CORE, 0),
    ];
    for (resource, cap) in limits {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: The getter fills the live structure with the current worker's limits.
        if unsafe { libc::getrlimit(resource, &mut limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
        limit.rlim_max = limit.rlim_max.min(cap);
        limit.rlim_cur = limit.rlim_cur.min(limit.rlim_max);
        // SAFETY: The setter reads the valid structure and only reduces this process's limits.
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observing_an_exited_pty_shell_leaves_it_waitable() {
        let mut pty = crate::Pty::spawn(80, 24).expect("start a real shell");
        let mut reader = pty.take_reader().unwrap();
        let drain = thread::spawn(move || io::copy(&mut reader, &mut io::sink()));
        pty.take_writer().unwrap().write_all(b"exit 47\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child_running(&pty.control.child).unwrap() {
            assert!(Instant::now() < deadline, "the shell did not exit");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!pty.any_job_process(|_, _| true));
        assert!(pty.shell_directory().is_err());
        assert!(!child_running(&pty.control.child).expect("shell is still waitable"));
        assert_eq!(pty.try_wait().unwrap().unwrap().code(), Some(47));
        pty.shutdown().unwrap();
        drain.join().unwrap().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn concurrent_master_opens_allocate_distinct_devices() {
        use std::collections::HashSet;
        use std::sync::Barrier;

        const COUNT: usize = 28;
        let start = Barrier::new(COUNT);
        let masters = thread::scope(|scope| {
            let threads = (0..COUNT)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        open_master().expect("allocate a PTY master")
                    })
                })
                .collect::<Vec<_>>();
            threads
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let slaves = masters
            .iter()
            .map(|master| slave_path(master.as_raw_fd()).expect("query its slave device"))
            .collect::<HashSet<_>>();
        assert_eq!(slaves.len(), COUNT);
    }

    #[test]
    fn forwards_the_full_pixel_range_to_the_kernel() {
        let mut pty = crate::Pty::spawn(80, 24).expect("start a real shell");
        pty.resize_with_pixels(98, 31, u16::MAX, 32768)
            .expect("resize in cells and pixels");
        let master = pty.control.master.as_ref().expect("open PTY master");
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is an open PTY and size is a writable winsize.
        let result = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
        os_result(result).expect("read terminal dimensions from the kernel");
        assert_eq!((size.ws_col, size.ws_row), (98, 31));
        assert_eq!((size.ws_xpixel, size.ws_ypixel), (u16::MAX, 32768));
        pty.shutdown().expect("reap shell");
    }
}
