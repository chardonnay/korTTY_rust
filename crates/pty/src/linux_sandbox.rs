//! Child-only Linux filesystem confinement. The policy is built before fork.

use std::env;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::ptr;

#[path = "linux_sandbox/seccomp.rs"]
mod seccomp;
#[path = "linux_sandbox/worker.rs"]
pub(crate) mod worker;

// Linux uapi/linux/landlock.h. ABI 6 supplies signal and abstract socket scopes.
const CREATE_VERSION: libc::c_uint = 1;
const MIN_ABI: libc::c_long = 6;
const RULE_PATH_BENEATH: libc::c_int = 1;
const SCOPE_ABSTRACT_UNIX: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;
const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const REMOVE_DIR: u64 = 1 << 4;
const REMOVE_FILE: u64 = 1 << 5;
const MAKE_DIR: u64 = 1 << 7;
const MAKE_REG: u64 = 1 << 8;
const MAKE_FIFO: u64 = 1 << 10;
const MAKE_SYM: u64 = 1 << 12;
const REFER: u64 = 1 << 13;
const TRUNCATE: u64 = 1 << 14;
const IOCTL_DEV: u64 = 1 << 15;
const HANDLED_FS: u64 = (1 << 16) - 1;
const READ_SYSTEM: u64 = EXECUTE | READ_FILE | READ_DIR;
const WRITE_WORKSPACE: u64 = READ_SYSTEM
    | WRITE_FILE
    | REMOVE_DIR
    | REMOVE_FILE
    | MAKE_DIR
    | MAKE_REG
    | MAKE_FIFO
    | MAKE_SYM
    | REFER
    | TRUNCATE;

const SYSTEM_DIRECTORIES: &[&str] = &[
    "/bin",
    "/usr/bin",
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/usr/libexec",
    "/usr/share/locale",
    "/usr/share/terminfo",
    "/usr/share/zoneinfo",
];

const SYSTEM_FILES: &[(&str, u64)] = &[
    ("/etc/ld.so.cache", READ_FILE),
    ("/etc/passwd", READ_FILE),
    ("/etc/group", READ_FILE),
    ("/etc/nsswitch.conf", READ_FILE),
    ("/etc/hosts", READ_FILE),
    ("/etc/resolv.conf", READ_FILE),
    ("/etc/localtime", READ_FILE),
    ("/dev/null", READ_FILE | WRITE_FILE),
    ("/dev/zero", READ_FILE),
    ("/dev/urandom", READ_FILE),
    ("/dev/tty", READ_FILE | WRITE_FILE | IOCTL_DEV),
];

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: libc::c_int,
}

pub(crate) struct Restrictions {
    workspace: File,
    ruleset: OwnedFd,
    filter: Vec<libc::sock_filter>,
    denied_probe: CString,
    profile_probe: Option<CString>,
}

impl Restrictions {
    /// Runs in the single-threaded child before exec. Uses only direct kernel calls.
    pub(crate) fn apply(&self) -> io::Result<()> {
        // SAFETY: both descriptors are owned by this object and survived fork.
        system_result(unsafe { libc::fchdir(self.workspace.as_raw_fd()) }.into())?;
        // Keep the std process launcher error pipe open until exec, but do not
        // let the shell inherit any desktop descriptor beyond its own PTY.
        close_for_exec()?;
        enforce(&self.ruleset)?;
        seccomp::apply(&self.filter)?;
        open_probe(c".", libc::O_DIRECTORY)?;
        require_denied(&self.denied_probe, 0)?;
        if let Some(profile) = &self.profile_probe {
            require_denied(profile, libc::O_DIRECTORY)?;
        }
        Ok(())
    }
}

pub(crate) fn prepare(
    workspace: &Path,
    profile: &Path,
) -> io::Result<(Restrictions, SessionDirectory)> {
    let uid = unprivileged_user()?;
    check_landlock_abi()?;
    let profile = canonical_protected_path(profile, false)?;
    let protected = protected_paths(&profile, uid)?;
    let workspace = fs::canonicalize(workspace)?;
    check_overlap(&workspace, &protected)?;
    let workspace_fd = path_handle(&workspace)?;
    let metadata = workspace_fd.metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the sandbox workspace must be a directory owned by the current user",
        ));
    }

    // The reference local shell retains its IP networking.
    let ruleset = create_ruleset(0)?;

    for path in SYSTEM_DIRECTORIES {
        allow_system_path(&ruleset, Path::new(path), READ_SYSTEM, &protected)?;
    }
    for (path, access) in SYSTEM_FILES {
        allow_system_path(&ruleset, Path::new(path), *access, &protected)?;
    }
    allow_path(&ruleset, &workspace_fd, WRITE_WORKSPACE)?;
    let directory = SessionDirectory::new()?;
    check_overlap(&directory.root, &protected)?;
    if intersects(&workspace, &directory.root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the workspace must be separate from the sandbox scratch directory",
        ));
    }
    allow_path(&ruleset, &path_handle(directory.path())?, WRITE_WORKSPACE)?;
    // The test file sits outside every allowed hierarchy. Prepare it without
    // writing any data into the user's profile or chosen workspace.
    let denied_probe = directory.denied_probe()?;
    let profile_probe = match fs::metadata(&profile) {
        Ok(metadata) if metadata.is_dir() => Some(probe_path(&profile)?),
        Ok(_) => return Err(io::Error::from(io::ErrorKind::NotADirectory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let restrictions = Restrictions {
        workspace: workspace_fd,
        ruleset,
        filter: seccomp::filter(),
        denied_probe,
        profile_probe,
    };
    Ok((restrictions, directory))
}

fn unprivileged_user() -> io::Result<libc::uid_t> {
    let (mut uid, mut effective, mut saved) = (0, 0, 0);
    // SAFETY: all three UID output pointers remain live and writable.
    system_result(unsafe { libc::getresuid(&mut uid, &mut effective, &mut saved) }.into())?;
    if uid == 0 || effective != uid || saved != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the sandbox requires an unprivileged desktop user",
        ));
    }
    Ok(uid)
}

fn check_landlock_abi() -> io::Result<()> {
    // SAFETY: the version query takes a null attr, size zero and the version flag.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            ptr::null::<RulesetAttr>(),
            0_usize,
            CREATE_VERSION,
        )
    };
    if abi < MIN_ABI {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the sandbox requires Linux Landlock ABI 6 or later",
        ));
    }
    Ok(())
}

fn create_ruleset(handled_access_net: u64) -> io::Result<OwnedFd> {
    let attr = RulesetAttr {
        handled_access_fs: HANDLED_FS,
        handled_access_net,
        scoped: SCOPE_ABSTRACT_UNIX | SCOPE_SIGNAL,
    };
    // SAFETY: attr has the Linux ABI layout and remains live during the syscall.
    let descriptor = system_result(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            ptr::from_ref(&attr),
            size_of::<RulesetAttr>(),
            0_u32,
        )
    })?;
    // SAFETY: successful ruleset creation returned a new owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor as libc::c_int) })
}

fn close_for_exec() -> io::Result<()> {
    // SAFETY: close_range takes only numeric arguments and marks CLOEXEC.
    system_result(unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    })
    .map(drop)
}

fn no_new_privileges() -> io::Result<()> {
    // SAFETY: the documented prctl operation takes one flag and three zeros.
    system_result(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1_u64, 0_u64, 0_u64, 0_u64) }.into(),
    )
    .map(drop)
}

fn enforce(ruleset: &OwnedFd) -> io::Result<()> {
    no_new_privileges()?;
    // SAFETY: this is our live ruleset descriptor. Enforcement is irreversible.
    system_result(unsafe {
        libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0_u32)
    })
    .map(drop)
}

fn allow_system_path(
    ruleset: &OwnedFd,
    path: &Path,
    access: u64,
    protected: &[PathBuf],
) -> io::Result<()> {
    let system_resolver = path == Path::new("/etc/resolv.conf");
    let path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    check_overlap(&path, protected)?;
    let fd = path_handle(&path)?;
    let metadata = fd.metadata()?;
    // systemd-resolved generates these exact read-only inputs under its own UID.
    let generated_resolver = system_resolver
        && matches!(
            path.to_str(),
            Some("/run/systemd/resolve/resolv.conf" | "/run/systemd/resolve/stub-resolv.conf")
        )
        && metadata.is_file()
        && metadata.mode() & 0o022 == 0;
    if metadata.uid() != 0 && !generated_resolver {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "sandbox system path is not owned by root ({})",
                path.display()
            ),
        ));
    }
    allow_path(ruleset, &fd, access)
}

fn allow_path(ruleset: &OwnedFd, file: &File, access: u64) -> io::Result<()> {
    let attr = PathBeneathAttr {
        allowed_access: access,
        parent_fd: file.as_raw_fd(),
    };
    // SAFETY: both live descriptors and the packed kernel rule remain valid during the call.
    system_result(unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset.as_raw_fd(),
            RULE_PATH_BENEATH,
            ptr::from_ref(&attr),
            0_u32,
        )
    })
    .map(drop)
}

fn path_handle(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
        .open(path)?;
    let file_type = file.metadata()?.file_type();
    if file_type.is_symlink()
        || fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))? != path
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a sandbox path changed while preparing it",
        ));
    }
    Ok(file)
}

fn intersects(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn check_overlap(path: &Path, protected: &[PathBuf]) -> io::Result<()> {
    if protected.iter().any(|secret| intersects(path, secret)) {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a sandbox allowlist path overlaps protected desktop data",
        ))
    } else {
        Ok(())
    }
}

fn protected_paths(profile: &Path, uid: libc::uid_t) -> io::Result<Vec<PathBuf>> {
    let mut protected = vec![profile.to_owned()];
    let homes = [login_home(uid)?, env::var_os("HOME").map(PathBuf::from)];
    for home in homes
        .into_iter()
        .flatten()
        .filter(|home| home.is_absolute())
    {
        for secret in [
            ".ssh",
            ".gnupg",
            "Library/Keychains",
            ".local/share/keyrings",
        ] {
            protected.push(canonical_protected_path(&home.join(secret), true)?);
        }
    }
    Ok(protected)
}

fn canonical_protected_path(path: &Path, allow_inaccessible_suffix: bool) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sandbox protected paths must be absolute and must not contain parent components",
        ));
    }
    let mut current = path;
    let mut missing = Vec::<&OsStr>::new();
    loop {
        match fs::canonicalize(current) {
            Ok(mut resolved) => {
                resolved.extend(missing.into_iter().rev());
                return Ok(resolved);
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || allow_inaccessible_suffix && error.raw_os_error() == Some(libc::EACCES) =>
            {
                // A home path already denied by DAC can keep its unsearchable
                // suffix for overlap checks. The child's Landlock rules add no access.
                let Some((name, parent)) = current.file_name().zip(current.parent()) else {
                    return Err(error);
                };
                if fs::symlink_metadata(current)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "cannot resolve a protected symbolic link",
                    ));
                }
                missing.push(name);
                current = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

fn login_home(uid: libc::uid_t) -> io::Result<Option<PathBuf>> {
    let mut entry = MaybeUninit::<libc::passwd>::uninit();
    let mut record = ptr::null_mut();
    let mut buffer = [0_u8; 16 * 1024];
    // SAFETY: the passwd, buffer and result pointer remain writable during the call.
    let error = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut record,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    if record.is_null() {
        return Ok(None);
    }
    // SAFETY: a successful non-null record initialized entry. pw_dir points
    // to a NUL-terminated string in the live buffer if provided.
    let directory = unsafe { entry.assume_init().pw_dir };
    if directory.is_null() {
        return Ok(None);
    }
    let home = unsafe { CStr::from_ptr(directory) };
    Ok(Some(PathBuf::from(OsStr::from_bytes(home.to_bytes()))))
}

pub(crate) struct SessionDirectory {
    root: PathBuf,
    scratch: PathBuf,
}

impl SessionDirectory {
    fn new() -> io::Result<Self> {
        let base = fs::canonicalize("/tmp")?;
        let metadata = base.metadata()?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the sandbox needs a trusted system temporary directory",
            ));
        }
        let mut random = [0_u8; 16];
        let mut received = 0;
        while received < random.len() {
            let buffer = &mut random[received..];
            // SAFETY: the writable buffer and exact size are valid for the call.
            let count = unsafe { libc::getrandom(buffer.as_mut_ptr().cast(), buffer.len(), 0) };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if count == 0 {
                return Err(io::Error::other(
                    "could not generate a sandbox scratch name",
                ));
            }
            received += count as usize;
        }
        let name = OsString::from(format!(
            "kortty-session-{:032x}",
            u128::from_ne_bytes(random)
        ));
        let root = base.join(name);
        let mut builder = DirBuilder::new();
        builder.mode(0o700).create(&root)?;
        let directory = Self {
            scratch: root.join("scratch"),
            root,
        };
        builder.create(&directory.scratch)?;
        Ok(directory)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.scratch
    }

    fn denied_probe(&self) -> io::Result<CString> {
        let path = self.root.join("denied-probe");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let path = probe_path(&path)?;
        open_probe(&path, 0)?;
        Ok(path)
    }
}

impl Drop for SessionDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn probe_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn require_denied(path: &CStr, flags: libc::c_int) -> io::Result<()> {
    match open_probe(path, flags) {
        Ok(()) => Err(io::Error::from_raw_os_error(libc::EPERM)),
        Err(error) if error.raw_os_error() == Some(libc::EACCES) => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_probe(path: &CStr, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: path is a live, NUL-terminated string. The flags do not create a file.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    system_result(fd.into())?;
    // SAFETY: open returned a descriptor that only this call owns.
    unsafe { libc::close(fd) };
    Ok(())
}

fn system_result(result: libc::c_long) -> io::Result<libc::c_long> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an unprivileged Linux user"]
    fn inaccessible_home_suffix_keeps_its_known_canonical_prefix() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let directory = SessionDirectory::new().unwrap();
        let blocked = directory.path().join("blocked");
        DirBuilder::new().mode(0o000).create(&blocked).unwrap();
        let alias = directory.path().join("alias");
        symlink(&blocked, &alias).unwrap();
        let protected = alias.join("keys");

        let home_path = canonical_protected_path(&protected, true);
        let profile_path = canonical_protected_path(&protected, false);
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(home_path.unwrap(), blocked.join("keys"));
        assert_eq!(profile_path.unwrap_err().raw_os_error(), Some(libc::EACCES));
    }
}
