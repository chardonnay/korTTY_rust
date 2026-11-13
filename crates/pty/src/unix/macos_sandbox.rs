//! Per-session Seatbelt policy for local macOS shells.

use std::env;
use std::ffi::{CStr, OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr;
use std::time::Duration;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const SHELL: &str = "/bin/sh";
const SYSTEM_PROGRAMS: &[&str] = &["/bin", "/usr/bin", "/usr/libexec"];
// The readable system directories and configuration files in local_shell.sbpl.
const SYSTEM_READ_PATHS: &[&str] = &[
    "/usr/lib",
    "/System/Library",
    "/private/var/db/dyld",
    "/usr/share/locale",
    "/usr/share/terminfo",
    "/usr/share/zoneinfo",
    "/private/etc/hosts",
    "/private/etc/resolv.conf",
    "/private/var/select/sh",
    "/private/etc/localtime",
    "/private/etc/passwd",
    "/private/etc/group",
];
const BACKEND: &str = "seatbelt";
const READY: &[u8] = b"kortty-local-pty-ready-v1\n";
const ACK: &[u8] = b"kortty-local-pty-start-v1\n";

// sandbox-exec first applies Seatbelt, then execs this fixed script with only
// literal positional arguments. The selected executable is never the probe.
const PROBE: &str = r#"
    [ -t 0 ] && /bin/ls -A . >/dev/null 2>&1 || exit 125
    if ( : < "$1" ) 2>/dev/null || ( : >> "$1" ) 2>/dev/null; then exit 125; fi
    if [ -n "$2" ] && /bin/ls -A "$2" >/dev/null 2>&1; then exit 125; fi
    (umask 077; : > "$3") || exit 125
    shift 3
    [ -f "$1" ] && [ -x "$1" ] || exit 125
    printf '%s\n' kortty-local-pty-ready-v1 >&2 || exit 125
    IFS= read -r start <&2 && [ "$start" = kortty-local-pty-start-v1 ] || exit 125
    exec "$@" 2>&1
"#;

const POLICY: &str = include_str!("local_shell.sbpl");

pub(super) struct Restrictions {
    workspace: File,
    path: PathBuf,
    profile_probe: Option<PathBuf>,
    protected: Vec<PathBuf>,
    descriptor_limit: libc::c_int,
}

impl Restrictions {
    pub(super) fn command(
        &self,
        directory: &SessionDirectory,
        program: &OsStr,
        tty: &Path,
    ) -> io::Result<(Command, UnixStream)> {
        let program = self.program(program, directory)?;
        let (ready, probe) = UnixStream::pair()?;
        ready.set_read_timeout(Some(Duration::from_secs(5)))?;
        ready.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut command = Command::new(SANDBOX_EXEC);
        command.args(["-p", POLICY]);
        // Parameters are separate argv values, so no file name is parsed as SBPL.
        parameter(&mut command, "WORKSPACE", &self.path);
        parameter(&mut command, "SCRATCH", directory.path());
        parameter(&mut command, "PTY", tty);
        command
            .args([SHELL, "-c", PROBE, "kortty-local-pty-probe"])
            .arg(directory.root.join("denied-probe"))
            .arg(self.profile_probe.as_deref().unwrap_or(Path::new("")))
            .arg(directory.path().join("write-probe"))
            .arg(program)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "en_US.UTF-8")
            .env("HOME", directory.path())
            .env("TMPDIR", directory.path())
            .env("HISTFILE", directory.path().join("history"))
            .env("KORTTY_SANDBOX", BACKEND)
            .stderr(Stdio::from(std::os::fd::OwnedFd::from(probe)));
        Ok((command, ready))
    }

    fn program(&self, program: &OsStr, directory: &SessionDirectory) -> io::Result<PathBuf> {
        let requested = Path::new(program);
        let path = if requested.is_absolute() {
            fs::canonicalize(requested)?
        } else if requested.components().count() == 1 {
            SYSTEM_PROGRAMS[..2]
                .iter()
                .find_map(|root| fs::canonicalize(Path::new(root).join(requested)).ok())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
        } else {
            fs::canonicalize(self.path.join(requested))?
        };
        let system = SYSTEM_PROGRAMS.iter().any(|root| path.starts_with(root));
        let metadata = path.metadata()?;
        if !(system || path.starts_with(&self.path) || path.starts_with(directory.path()))
            || (system && (metadata.uid() != 0 || metadata.mode() & 0o022 != 0))
            || !metadata.is_file()
            || metadata.mode() & 0o111 == 0
            || metadata.mode() & 0o6000 != 0
        {
            return Err(permission(
                "the shell executable is outside the approved workspace and system programs",
            ));
        }
        check_overlap(&path, &self.protected)?;
        Ok(path)
    }

    /// Uses only system calls in the single-threaded child before exec.
    pub(super) fn apply(&self) -> io::Result<()> {
        // SAFETY: the workspace descriptor remains owned by this object.
        super::os_result(unsafe { libc::fchdir(self.workspace.as_raw_fd()) })?;
        // Keep the process launcher's error pipe usable until exec. No desktop
        // descriptor outside stdio may survive into sandbox-exec or the target.
        for fd in 3..self.descriptor_limit {
            // SAFETY: fcntl checks whether this number is a live descriptor.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBADF) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub(super) fn attest(&self, mut ready: UnixStream) -> io::Result<()> {
        let mut marker = [0; READY.len()];
        if ready.read_exact(&mut marker).is_err() || marker.as_slice() != READY {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "macOS Seatbelt did not pass the local shell containment check",
            ));
        }
        let current = workspace_handle(&self.path)?;
        if identity(&self.workspace.metadata()?) != identity(&current.metadata()?) {
            return Err(permission("the sandbox workspace changed during launch"));
        }
        check_overlap(&self.path, &self.protected)?;
        ready.write_all(ACK)
    }
}

pub(super) fn prepare(
    workspace: &Path,
    profile: &Path,
) -> io::Result<(Restrictions, SessionDirectory)> {
    // SAFETY: these calls return the current process credentials and state.
    let (uid, effective, privileged) =
        unsafe { (libc::getuid(), libc::geteuid(), libc::issetugid()) };
    if uid == 0 || uid != effective || privileged != 0 {
        return Err(permission(
            "the sandbox requires an unprivileged desktop user",
        ));
    }
    let profile = canonical_protected(profile)?;
    let profile_probe = match profile.metadata() {
        Ok(metadata) if metadata.is_dir() => Some(profile.clone()),
        Ok(_) => return Err(io::Error::from(io::ErrorKind::NotADirectory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut protected = vec![profile];
    let homes = [login_home(uid)?, env::var_os("HOME").map(PathBuf::from)];
    for home in homes
        .into_iter()
        .flatten()
        .filter(|home| home.is_absolute())
    {
        for name in [
            ".ssh",
            ".gnupg",
            "Library/Keychains",
            ".local/share/keyrings",
        ] {
            protected.push(canonical_protected(&home.join(name))?);
        }
    }
    let path = fs::canonicalize(workspace)?;
    check_overlap(&path, &protected)?;
    check_directory_chain(&path, uid)?;
    let workspace = workspace_handle(&path)?;
    if workspace.metadata()?.uid() != uid {
        return Err(permission(
            "the sandbox workspace must be owned by the current user",
        ));
    }
    for path in [SANDBOX_EXEC, SHELL] {
        let tool = fs::metadata(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("macOS sandbox system tool {path} is unavailable"),
            )
        })?;
        if !tool.is_file()
            || tool.uid() != 0
            || tool.mode() & 0o022 != 0
            || tool.mode() & 0o111 == 0
        {
            return Err(permission("a macOS sandbox system tool is not trusted"));
        }
    }
    for root in SYSTEM_PROGRAMS.iter().chain(SYSTEM_READ_PATHS) {
        let system_path = match fs::canonicalize(root) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        check_overlap(&system_path, &protected)?;
        let info = system_path.metadata()?;
        if info.uid() != 0 || info.mode() & 0o022 != 0 {
            return Err(permission(
                "a macOS sandbox system path is writable by users",
            ));
        }
        if let Some(parent) = system_path.parent() {
            check_directory_chain(parent, 0)?;
        }
    }
    let directory = SessionDirectory::new(uid, &path, &protected)?;
    let restrictions = Restrictions {
        workspace,
        path,
        profile_probe,
        protected,
        descriptor_limit: descriptor_limit()?,
    };
    Ok((restrictions, directory))
}

fn parameter(command: &mut Command, name: &str, path: &Path) {
    let mut value = OsString::from(name);
    value.push("=");
    value.push(path.as_os_str());
    command.arg("-D").arg(value);
}

fn descriptor_limit() -> io::Result<libc::c_int> {
    let mut limit: libc::c_int = 0;
    let mut size = size_of_val(&limit);
    // SAFETY: the exact-sized output and its size pointer are live.
    super::os_result(unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            ptr::from_mut(&mut limit).cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    })?;
    if size != size_of_val(&limit) || limit <= 3 {
        return Err(io::Error::other(
            "could not determine the macOS descriptor ceiling",
        ));
    }
    Ok(limit)
}

fn workspace_handle(path: &Path) -> io::Result<File> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    if identity(&directory.metadata()?) != identity(&path.symlink_metadata()?) {
        return Err(permission("a sandbox path changed while preparing it"));
    }
    Ok(directory)
}

fn canonical_protected(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox protected paths must be absolute without parent components",
        ));
    }
    let mut current = path;
    let mut suffix = Vec::new();
    loop {
        match fs::canonicalize(current) {
            Ok(mut resolved) => {
                resolved.extend(suffix.into_iter().rev());
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some((name, parent)) = current.file_name().zip(current.parent()) else {
                    return Err(error);
                };
                if current
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.is_symlink())
                {
                    return Err(permission("cannot resolve a protected symbolic link"));
                }
                suffix.push(name);
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

fn check_overlap(path: &Path, protected: &[PathBuf]) -> io::Result<()> {
    for secret in protected {
        if within(path, secret) || within(secret, path) {
            return Err(permission("a sandbox path overlaps protected desktop data"));
        }
    }
    Ok(())
}

fn within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
        // Detect existing case and APFS firmlink aliases by filesystem identity.
        || root.metadata().is_ok_and(|metadata| {
            path.ancestors().any(|ancestor| {
                ancestor
                    .metadata()
                    .is_ok_and(|entry| identity(&entry) == identity(&metadata))
            })
        })
}

fn check_directory_chain(path: &Path, uid: libc::uid_t) -> io::Result<()> {
    for ancestor in path.ancestors() {
        let info = ancestor.symlink_metadata()?;
        let owner = info.uid();
        let mode = info.mode();
        // macOS stores its generated resolv.conf in this root:daemon directory.
        let system_run = uid == 0
            && ancestor == Path::new("/private/var/run")
            && owner == 0
            && info.gid() == 1
            && mode & 0o002 == 0;
        if !info.is_dir()
            || (owner != 0 && owner != uid)
            || (mode & 0o022 != 0 && !(system_run || owner == 0 && mode & 0o1000 != 0))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "a macOS sandbox path has an untrusted directory: {}",
                    ancestor.display()
                ),
            ));
        }
    }
    Ok(())
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn login_home(uid: libc::uid_t) -> io::Result<Option<PathBuf>> {
    let mut entry = MaybeUninit::<libc::passwd>::uninit();
    let mut record = ptr::null_mut();
    let mut buffer = [0_u8; 16 * 1024];
    // SAFETY: the record and backing buffer are live and writable.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut record,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if record.is_null() {
        return Ok(None);
    }
    // SAFETY: a successful non-null result initializes entry and its strings
    // are NUL terminated within the live buffer.
    let home = unsafe { entry.assume_init().pw_dir };
    if home.is_null() {
        return Ok(None);
    }
    let home = unsafe { CStr::from_ptr(home) };
    Ok(Some(PathBuf::from(OsStr::from_bytes(home.to_bytes()))))
}

pub(super) struct SessionDirectory {
    root: PathBuf,
    scratch: PathBuf,
    identity: (u64, u64),
}

impl SessionDirectory {
    fn new(uid: libc::uid_t, workspace: &Path, protected: &[PathBuf]) -> io::Result<Self> {
        let base = fs::canonicalize(env::temp_dir())?;
        check_directory_chain(&base, uid)?;
        if protected.iter().any(|path| within(&base, path)) || within(&base, workspace) {
            return Err(permission(
                "the temporary directory overlaps a protected sandbox path",
            ));
        }
        let mut template = std::ffi::CString::new(
            base.join("kortty-local-pty.XXXXXX")
                .into_os_string()
                .as_bytes(),
        )?
        .into_bytes_with_nul();
        // SAFETY: mkdtemp writes inside this mutable NUL-terminated buffer.
        let path = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        if path.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful call returns the live NUL-terminated template.
        let root = PathBuf::from(OsStr::from_bytes(
            unsafe { CStr::from_ptr(path) }.to_bytes(),
        ));
        let metadata = root.symlink_metadata()?;
        let directory = Self {
            scratch: root.join("scratch"),
            root,
            identity: identity(&metadata),
        };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(permission("could not prepare a private sandbox directory"));
        }
        check_overlap(&directory.root, protected)?;
        if within(&directory.root, workspace) || within(workspace, &directory.root) {
            return Err(permission(
                "the workspace must be separate from the sandbox scratch directory",
            ));
        }
        DirBuilder::new().mode(0o700).create(&directory.scratch)?;
        let denied_probe = directory.root.join("denied-probe");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&denied_probe)?;
        // The probe must be accessible before we apply the policy.
        OpenOptions::new()
            .read(true)
            .append(true)
            .open(denied_probe)?;
        Ok(directory)
    }

    fn path(&self) -> &Path {
        &self.scratch
    }

    pub(super) fn backend(&self) -> &'static str {
        BACKEND
    }
}

impl Drop for SessionDirectory {
    fn drop(&mut self) {
        if self
            .root
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir() && identity(&metadata) == self.identity)
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn permission(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
