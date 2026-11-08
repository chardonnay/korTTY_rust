use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Child;

#[derive(Clone, Copy)]
pub(super) struct Executable {
    program: FileId,
    #[cfg(target_os = "macos")]
    system_sh: Option<FileId>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileId(u64, u64);

impl Executable {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        Ok(Self {
            program: FileId::read(path)?,
            // macOS /bin/sh dispatches to the shell selected by the system.
            #[cfg(target_os = "macos")]
            system_sh: (path == Path::new("/bin/sh"))
                .then(|| FileId::read(Path::new("/private/var/select/sh")).ok())
                .flatten(),
        })
    }
}

impl FileId {
    fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self(metadata.dev(), metadata.ino()))
    }
}

pub(super) fn resolve(
    child: &mut Child,
    master: &File,
    executable: Executable,
) -> io::Result<PathBuf> {
    let pid = i32::try_from(child.id()).map_err(|_| unverified())?;
    if child.try_wait()?.is_some() {
        return Err(unverified());
    }
    #[cfg(target_os = "linux")]
    if !crate::process_usage::procfs_uses_our_pid_namespace() {
        return Err(unverified());
    }
    verify(pid, master, executable)?;
    let path = directory(pid)?;
    verify(pid, master, executable)?;
    if child.try_wait()?.is_some() || !path.is_absolute() {
        return Err(unverified());
    }
    Ok(path)
}

pub(super) fn unverified() -> io::Error {
    io::Error::other("the original local shell and its working directory could not be verified")
}

#[cfg(target_os = "linux")]
fn verify(pid: i32, master: &File, executable: Executable) -> io::Result<()> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;

    // SAFETY: master remains open during the tty query.
    if unsafe { libc::tcgetpgrp(master.as_raw_fd()) } != pid
        || FileId::read(Path::new(&format!("/proc/{pid}/exe")))? != executable.program
    {
        return Err(unverified());
    }
    // A shell without job control can leave the child in its own foreground group.
    let mut children = String::new();
    fs::File::open(format!("/proc/{pid}/task/{pid}/children"))?
        .take(4097)
        .read_to_string(&mut children)?;
    if children.len() > 4096 {
        return Err(unverified());
    }
    for child in children.split_ascii_whitespace() {
        let child = child.parse::<libc::pid_t>().map_err(|_| unverified())?;
        // SAFETY: getpgid reads the process group of a numeric PID.
        if unsafe { libc::getpgid(child) } == pid {
            return Err(unverified());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn directory(pid: i32) -> io::Result<PathBuf> {
    fs::read_link(format!("/proc/{pid}/cwd"))
}

#[cfg(target_os = "macos")]
macro_rules! macos_info {
    ($pid:expr, $flavor:expr, $output:ty) => {{
        let mut output = std::mem::MaybeUninit::<$output>::uninit();
        let size = std::mem::size_of::<$output>();
        // SAFETY: the flavor selects the matching C struct and the buffer stays writable.
        let written = unsafe {
            libc::proc_pidinfo($pid, $flavor, 0, output.as_mut_ptr().cast(), size as i32)
        };
        if written == size as i32 {
            // SAFETY: proc_pidinfo filled the entire matching structure.
            Ok(unsafe { output.assume_init() })
        } else {
            Err(unverified())
        }
    }};
}

#[cfg(target_os = "macos")]
fn verify(pid: i32, _master: &File, executable: Executable) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let info = macos_info!(pid, libc::PROC_PIDTBSDINFO, libc::proc_bsdinfo)?;
    // SAFETY: geteuid takes no arguments.
    let euid = unsafe { libc::geteuid() };
    if info.pbi_uid != euid || info.pbi_pgid != pid as u32 || info.e_tpgid != pid as u32 {
        return Err(unverified());
    }
    let mut foreground = [0_i32; 2];
    // SAFETY: the buffer has two process IDs of writable storage.
    let count = unsafe {
        libc::proc_listpgrppids(
            pid,
            foreground.as_mut_ptr().cast(),
            std::mem::size_of_val(&foreground) as i32,
        )
    };
    if count != 1 || foreground[0] != pid {
        return Err(unverified());
    }
    let mut path = [0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: path owns the writable buffer of the given size for the entire call.
    let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    let end = path
        .iter()
        .position(|&byte| byte == 0)
        .filter(|&end| end > 0 && length > 0)
        .ok_or_else(unverified)?;
    let path = Path::new(std::ffi::OsStr::from_bytes(&path[..end]));
    let actual = FileId::read(path)?;
    if !path.is_absolute() || (actual != executable.program && Some(actual) != executable.system_sh)
    {
        return Err(unverified());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn directory(pid: i32) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;

    let info = macos_info!(pid, libc::PROC_PIDVNODEPATHINFO, libc::proc_vnodepathinfo)?;
    let bytes = info.pvi_cdir.vip_path.as_flattened();
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(unverified)?;
    Ok(std::ffi::OsString::from_vec(bytes[..end].iter().map(|&byte| byte as u8).collect()).into())
}
