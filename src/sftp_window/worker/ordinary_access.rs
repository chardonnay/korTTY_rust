//! Ordinary local creation rights for Scheduler downloads, independent of server modes.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::sftp_window::local_tree::Directory;
use crate::sftp_window::worker::resume::persist::Identity;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
use kortty_files::NewFileAccess as NativeAccess;
#[cfg(unix)]
use unix::NativeAccess;

pub(in crate::sftp_window) struct Access {
    parent: PathBuf,
    identity: Identity,
    native: NativeAccess,
}

impl Access {
    pub(in crate::sftp_window) fn capture(parent: &Path, identity: &Identity) -> io::Result<Self> {
        let directory = Directory::open_path(parent)?;
        identity.require_directory(&directory)?;
        #[cfg(unix)]
        let native = NativeAccess::capture(&directory)?;
        #[cfg(windows)]
        let native = NativeAccess::capture_ordinary(&directory)?;
        let access = Self {
            parent: parent.to_owned(),
            identity: identity.clone(),
            native,
        };
        access.verify_parent()?;
        Ok(access)
    }

    pub(in crate::sftp_window) fn capture_directory(
        parent: &Path,
        identity: &Identity,
    ) -> io::Result<Self> {
        let directory = Directory::open_path(parent)?;
        identity.require_directory(&directory)?;
        #[cfg(unix)]
        let native = NativeAccess::capture_directory(&directory)?;
        #[cfg(windows)]
        let native = NativeAccess::capture_ordinary_directory(&directory)?;
        let access = Self {
            parent: parent.to_owned(),
            identity: identity.clone(),
            native,
        };
        access.verify_parent()?;
        Ok(access)
    }

    pub(in crate::sftp_window) fn verify_parent(&self) -> io::Result<()> {
        let directory = Directory::open_path(&self.parent)?;
        self.identity.require_directory(&directory)?;
        self.native.require_parent(&directory)
    }

    pub(in crate::sftp_window) fn apply_to(&self, file: &File) -> io::Result<()> {
        self.verify_parent()?;
        self.native.apply_to(file)?;
        self.verify_parent()
    }

    pub(in crate::sftp_window) fn apply_to_directory(
        &self,
        directory: &Directory,
    ) -> io::Result<()> {
        self.verify_parent()?;
        #[cfg(unix)]
        self.native.apply_to_directory(directory.file())?;
        #[cfg(windows)]
        self.native.apply_to_directory(directory)?;
        self.verify_parent()
    }
}

#[cfg(all(test, unix))]
mod tests;
