use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::protocol::PROTOCOL_VERSION;

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub(crate) use windows::bind_loopback_exclusive;

#[cfg(unix)]
pub(crate) const SOCKET: &str = "control.sock";
const DESCRIPTOR: &str = "endpoint.json";
const MAX_DESCRIPTOR: u64 = 4096;

/// Profile location shared with the native desktop. The Java profile remains separate.
pub fn profile_root() -> io::Result<PathBuf> {
    let home = || env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|path| path.join("Library/Application Support"))
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home().map(|path| path.join(".config")))
    };
    base.filter(|path| path.is_absolute())
        .map(|path| path.join("kortty"))
        .ok_or_else(|| io::Error::other("kortty profile directory is unavailable"))
}

pub(crate) fn directory(root: &Path, create: bool) -> io::Result<PathBuf> {
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Control profile path must be absolute",
        ));
    }
    let path = root.join("control");
    if create {
        fs::create_dir_all(root)?;
        create_private_directory(&path)?;
    }
    verify_private_directory(&path)?;
    fs::canonicalize(path)
}

pub(crate) fn lock(directory: &Path) -> io::Result<File> {
    let path = directory.join("listener.lock");
    let file = match open_private_file(&path, true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private_file(&path, false)?
        }
        Err(error) => return Err(error),
    };
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

/// Token-bearing descriptor. Debug formatting is deliberately unavailable.
#[derive(Serialize, Deserialize)]
pub(crate) struct Endpoint {
    pub transport: String,
    pub path: Option<PathBuf>,
    pub host: Option<String>,
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub app_version: String,
    pub protocol_version: u8,
    pub instance_id: String,
    pub started_at_millis: u64,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl Endpoint {
    pub fn read_profile(root: &Path) -> io::Result<Self> {
        Self::read(&directory(root, false)?)
    }

    pub fn read(directory: &Path) -> io::Result<Self> {
        let file = open_private_file(&directory.join(DESCRIPTOR), false)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_DESCRIPTOR + 1).read_to_end(&mut bytes)?;
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid control endpoint descriptor",
            )
        };
        if bytes.len() as u64 > MAX_DESCRIPTOR {
            return Err(invalid());
        }
        let endpoint: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if endpoint.protocol_version != PROTOCOL_VERSION
            || endpoint.pid == 0
            || !(32..=256).contains(&endpoint.token.len())
            || !endpoint
                .token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid());
        }
        match endpoint.transport.as_str() {
            #[cfg(unix)]
            "unix" if endpoint.host.is_none() && endpoint.port == 0 => {
                use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
                let path = directory.join(SOCKET);
                if endpoint.path.as_deref() != Some(path.as_path()) {
                    return Err(invalid());
                }
                let metadata = fs::symlink_metadata(path)?;
                if !metadata.file_type().is_socket()
                    || metadata.uid() != rustix::process::geteuid().as_raw()
                {
                    return Err(invalid());
                }
            }
            #[cfg(windows)]
            "loopback"
                if endpoint.path.is_none()
                    && endpoint.host.as_deref() == Some("127.0.0.1")
                    && endpoint.port != 0 => {}
            _ => return Err(invalid()),
        }
        Ok(endpoint)
    }

    pub fn publish(&self, directory: &Path) -> io::Result<()> {
        let destination = directory.join(DESCRIPTOR);
        match open_private_file(&destination, false) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let temporary = directory.join(format!(".endpoint-{}.tmp", self.instance_id));
        let mut file = open_private_file(&temporary, true)?;
        let bytes = Zeroizing::new(serde_json::to_vec(self).map_err(io::Error::other)?);
        let result = file.write_all(&bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| fs::rename(&temporary, destination));
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

pub(crate) fn remove_descriptor(directory: &Path) {
    let _ = fs::remove_file(directory.join(DESCRIPTOR));
}

pub(crate) fn new_audit(directory: &Path, instance: &str) -> io::Result<File> {
    open_private_file(&directory.join(format!("audit-{instance}.log")), true)
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

#[cfg(unix)]
fn verify_private_directory(path: &Path) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags, open};

    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_dir() || !private(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Control directory must be private to the current user",
        ));
    }
    #[cfg(target_os = "macos")]
    verify_no_extended_acl(&file)?;
    Ok(())
}

#[cfg(unix)]
fn open_private_file(path: &Path, write_new: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    let mode = if write_new {
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
    } else {
        OFlags::RDONLY
    };
    let file = File::from(open(
        path,
        mode | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() || !private(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Control file must be private to the current user",
        ));
    }
    #[cfg(target_os = "macos")]
    verify_no_extended_acl(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn private(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    metadata.mode() & 0o077 == 0 && metadata.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn verify_no_extended_acl(file: &File) -> io::Result<()> {
    use std::ffi::{c_int, c_uint, c_void};
    use std::os::fd::AsRawFd as _;

    const ACL_TYPE_EXTENDED: c_uint = 0x100;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_uint) -> *mut c_void;
        fn acl_free(acl: *mut c_void) -> c_int;
    }

    // SAFETY: `file` keeps the already validated descriptor open for the call.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // Darwin reports ENOENT when the open file has no FILESEC_ACL property.
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(error);
    }
    // SAFETY: A non-null result is a fresh ACL allocation owned by this call.
    unsafe { acl_free(acl) };
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Control endpoint must be private to the current user (extended ACL present)",
    ))
}

#[cfg(windows)]
use windows::{create_private_directory, open_private_file, verify_private_directory};
