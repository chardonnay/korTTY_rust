//! Start the independently signed macOS App Sandbox helper with private IPC.

use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, PipeReader, PipeWriter};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::ptr::{NonNull, null_mut};
use std::time::Duration;

use crate::parent_exit::EXPECTED_PARENT;

mod signature;
mod spawn;

const TRANSPORT: RawFd = 3;
const CANARY: &str = "KORTTY_MAC_SSH_CANARY";
const PROBE: &str = "KORTTY_MAC_SSH_NETWORK_PROBE";
const PROFILE: &str = "KORTTY_MAC_SSH_PRIVATE_PROFILE";

/// Private control channels and a local byte stream to the confined worker.
pub struct MacWorker {
    pub process: MacWorkerProcess,
    pub input: PipeWriter,
    pub output: PipeReader,
    pub ciphertext: UnixStream,
}

/// Owns the child PID, its lifetime channel and its temporary startup probes.
pub struct MacWorkerProcess {
    pid: libc::pid_t,
    reaped: bool,
    signature: signature::ExpectedSignature,
    probes: Option<Probes>,
    _lifetime: PipeWriter,
}

impl MacWorker {
    /// Locates the signed helper next to the desktop's bundle resources.
    pub fn check_available(executable: &Path) -> io::Result<()> {
        Self::helper(executable).map(|_| ())
    }

    fn helper(executable: &Path) -> io::Result<PathBuf> {
        let unsupported = || {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "SSH App Sandbox requires the packaged macOS application",
            )
        };
        if !executable.is_absolute()
            || executable.file_name() != Some(OsStr::new("kortty"))
            || !executable.is_file()
        {
            return Err(unsupported());
        }
        let contents = executable
            .parent()
            .filter(|parent| parent.ends_with("Contents/MacOS"))
            .and_then(Path::parent)
            .ok_or_else(unsupported)?;
        let helper =
            contents.join("Helpers/kortty-ssh-worker.app/Contents/MacOS/kortty-ssh-worker");
        if !helper.is_file() {
            return Err(unsupported());
        }
        Ok(helper)
    }

    /// Starts a worker with no TCP socket. Wait for readiness and verify the
    /// running image before dialing or sending credentials.
    pub fn prepare(executable: &Path, argument: &str, profile: &Path) -> io::Result<Self> {
        let executable = Self::helper(executable)?;
        let signature = signature::before_spawn(&executable)?;
        let probes = Probes::new()?;
        let (child_input, input) = io::pipe()?;
        let (output, child_output) = io::pipe()?;
        let (child_lifetime, lifetime) = io::pipe()?;
        let (ciphertext, child_ciphertext) = UnixStream::pair()?;
        let mut env = vec![
            CString::new(format!("{EXPECTED_PARENT}={}", std::process::id()))?,
            env_path(CANARY, &probes.canary.file)?,
            CString::new(format!("{PROBE}={}", probes.listener.local_addr()?))?,
        ];
        match profile.canonicalize() {
            Ok(path) => env.push(env_path(PROFILE, &path)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let pid = spawn::launch(
            &executable,
            argument,
            TRANSPORT,
            [
                child_input.as_raw_fd(),
                child_output.as_raw_fd(),
                child_lifetime.as_raw_fd(),
                child_ciphertext.as_raw_fd(),
            ],
            &env,
        )?;
        let process = MacWorkerProcess {
            pid,
            reaped: false,
            signature,
            probes: Some(probes),
            _lifetime: lifetime,
        };
        process.resume()?;
        Ok(Self {
            process,
            input,
            output,
            ciphertext,
        })
    }

    /// Checks that App Sandbox denied protected paths and new network access.
    /// The reserved descriptor must identify an anonymous connected socket.
    pub fn enter(descriptor: u32) -> io::Result<UnixStream> {
        if descriptor != TRANSPORT as u32 {
            return Err(invalid("invalid macOS SSH relay descriptor"));
        }
        std::env::set_current_dir("/")?;
        require_pipe(libc::STDIN_FILENO, libc::O_RDONLY)?;
        require_pipe(libc::STDOUT_FILENO, libc::O_WRONLY)?;
        require_pipe(libc::STDERR_FILENO, libc::O_RDONLY)?;
        // SAFETY: fcntl validates this numeric descriptor. Duplication does
        // not assume ownership of the original descriptor inherited at exec.
        let fd = unsafe { libc::fcntl(TRANSPORT, libc::F_DUPFD_CLOEXEC, 4) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fcntl returned a fresh owned descriptor.
        let stream = UnixStream::from(unsafe { OwnedFd::from_raw_fd(fd) });
        if !stream.local_addr()?.is_unnamed() || !stream.peer_addr()?.is_unnamed() {
            return Err(invalid("SSH relay must be an anonymous Unix socket pair"));
        }
        // SAFETY: setting close-on-exec does not adopt or close this live descriptor.
        if unsafe { libc::fcntl(TRANSPORT, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let file = std::env::var_os(CANARY).ok_or_else(|| invalid("missing SSH file probe"))?;
        require_denied(
            File::open(&file),
            "SSH App Sandbox allowed reading the probe",
        )?;
        require_denied(
            OpenOptions::new().write(true).open(&file),
            "SSH App Sandbox allowed writing the probe",
        )?;
        if let Some(profile) = std::env::var_os(PROFILE) {
            require_denied(
                fs::read_dir(profile),
                "SSH App Sandbox allowed access to the desktop profile",
            )?;
        }
        let peer: SocketAddrV4 = std::env::var(PROBE)
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|address: &SocketAddrV4| address.ip().is_loopback() && address.port() > 0)
            .ok_or_else(|| invalid("invalid SSH network probe"))?;
        require_denied(
            TcpStream::connect_timeout(&peer.into(), Duration::from_secs(2)),
            "SSH App Sandbox allowed a new TCP connection",
        )?;
        require_denied(
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)),
            "SSH App Sandbox allowed TCP listening",
        )?;
        require_denied(
            UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)),
            "SSH App Sandbox allowed UDP binding",
        )?;
        Ok(stream)
    }
}

impl MacWorkerProcess {
    fn resume(&self) -> io::Result<()> {
        self.signature.verify_running(self.pid)?;
        // SAFETY: this process owns the just-launched suspended child PID.
        if unsafe { libc::kill(self.pid, libc::SIGCONT) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Verifies the live child's signature after its successful OS probes.
    pub fn verify(&mut self) -> io::Result<()> {
        self.signature.verify_running(self.pid)?;
        let _ = self.probes.take();
        Ok(())
    }

    pub fn id(&self) -> u32 {
        self.pid as u32
    }

    /// Measures the sandboxed child while its PID is still owned here.
    pub fn usage(&mut self) -> crate::ProcessUsage {
        if matches!(self.exited(), Ok(false)) {
            crate::process_usage::pid_usage(self.id())
        } else {
            crate::ProcessUsage::default()
        }
    }

    pub fn exited(&mut self) -> io::Result<bool> {
        self.reap(libc::WNOHANG)
    }

    pub fn kill(&mut self) -> io::Result<()> {
        if self.exited()? {
            return Ok(());
        }
        // SAFETY: the PID is this process's own unreaped child and cannot be reused yet.
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn wait(&mut self) -> io::Result<()> {
        self.reap(0).map(|_| ())
    }

    fn reap(&mut self, flags: libc::c_int) -> io::Result<bool> {
        while !self.reaped {
            // SAFETY: the PID is our own child. We only need the exit notification.
            let result = unsafe { libc::waitpid(self.pid, null_mut(), flags) };
            if result == 0 {
                return Ok(false);
            }
            if result == self.pid {
                self.reaped = true;
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.reaped = true;
            }
            return Err(error);
        }
        Ok(true)
    }
}

impl Drop for MacWorkerProcess {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}

struct Probes {
    canary: Canary,
    listener: TcpListener,
}

impl Probes {
    fn new() -> io::Result<Self> {
        Ok(Self {
            canary: Canary::new()?,
            listener: TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?,
        })
    }
}

struct Canary {
    directory: PathBuf,
    file: PathBuf,
}

impl Canary {
    fn new() -> io::Result<Self> {
        let mut template = b"/private/tmp/kortty-ssh-sandbox.XXXXXX\0".to_vec();
        // SAFETY: mkdtemp writes only to this mutable, NUL-terminated template.
        let path = NonNull::new(unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) })
            .ok_or_else(io::Error::last_os_error)?;
        // SAFETY: successful mkdtemp returned the same live, NUL-terminated buffer.
        let directory = PathBuf::from(OsStr::from_bytes(
            unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes(),
        ));
        let canary = Self {
            file: directory.join("access-probe"),
            directory,
        };
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&canary.file)?;
        Ok(canary)
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.file);
        let _ = fs::remove_dir(&self.directory);
    }
}

fn env_path(key: &str, path: &Path) -> io::Result<CString> {
    let mut value = key.as_bytes().to_vec();
    value.push(b'=');
    value.extend_from_slice(path.as_os_str().as_bytes());
    Ok(CString::new(value)?)
}

fn require_denied<T>(operation: io::Result<T>, message: &str) -> io::Result<()> {
    match operation {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Err(error) => Err(io::Error::new(error.kind(), format!("{message}: {error}"))),
        Ok(_) => Err(io::Error::new(io::ErrorKind::PermissionDenied, message)),
    }
}

fn require_pipe(fd: RawFd, mode: libc::c_int) -> io::Result<()> {
    // SAFETY: both calls validate the numeric descriptor. fstat initializes
    // its output only on success.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the output slot has space and alignment for a native stat.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized the record.
    let stat = unsafe { stat.assume_init() };
    if flags & libc::O_ACCMODE != mode || stat.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Err(invalid("macOS SSH worker requires private control pipes"));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
