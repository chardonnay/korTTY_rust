//! New browser downloads follow Java's local finalizer permission rules.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::sftp_window::local_tree::Directory;
use crate::sftp_window::worker::resume::persist::Identity;

#[cfg(unix)]
use super::sync::replacement::metadata::unix::Metadata as NativeAccess;
#[cfg(windows)]
use kortty_files::NewFileAccess as NativeAccess;

pub(in crate::sftp_window) struct Access {
    parent: PathBuf,
    identity: Identity,
    native: NativeAccess,
}

impl Access {
    pub(in crate::sftp_window) fn capture(
        parent: &Path,
        identity: &Identity,
        remote_mode: Option<u32>,
    ) -> io::Result<Self> {
        let directory = Directory::open_path(parent)?;
        identity.require_directory(&directory)?;
        #[cfg(unix)]
        let native = NativeAccess::for_new_file(remote_mode);
        #[cfg(windows)]
        let native = {
            let _ = remote_mode;
            NativeAccess::capture(&directory)?
        };
        Ok(Self {
            parent: parent.to_owned(),
            identity: identity.clone(),
            native,
        })
    }

    pub(in crate::sftp_window) fn verify_parent(&self) -> io::Result<()> {
        let directory = Directory::open_path(&self.parent)?;
        self.identity.require_directory(&directory)?;
        #[cfg(windows)]
        self.native.require_parent(&directory)?;
        Ok(())
    }

    pub(in crate::sftp_window) fn apply_to(&self, file: &File) -> io::Result<()> {
        self.verify_parent()?;
        self.native.apply_to(file)?;
        self.verify_parent()
    }
}
