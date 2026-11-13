//! Native local source identity and digest for durable staged uploads.

use serde::{Deserialize, Serialize};

use crate::sftp_window::worker::resume::persist::{Identity, LocalPath};

use super::*;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct UploadSource {
    pub(in crate::sftp_window) path: LocalPath,
    pub(in crate::sftp_window) parent: Identity,
    pub(in crate::sftp_window) file: Identity,
    pub(in crate::sftp_window) size: u64,
    pub(in crate::sftp_window) sha256: String,
}

impl LocalSource {
    pub(in crate::sftp_window) fn upload_source(
        &self,
        cancelled: &impl Fn() -> bool,
    ) -> io::Result<UploadSource> {
        self.verify()?;
        let parent = Identity::directory(&self.parent)?;
        let file = Identity::capture(&self.file)?;
        let size = self.size();
        let sha256 = super::resume::prefix(&self.file, size, cancelled)?;
        self.verify()?;
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(UploadSource {
            path: LocalPath::capture(&self.path),
            parent,
            file,
            size,
            sha256,
        })
    }

    pub(in crate::sftp_window) fn reopen_upload(
        saved: &UploadSource,
        cancelled: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let source = Self::open(&saved.path.path()?)?;
        saved.parent.require_directory(&source.parent)?;
        saved.file.require(&source.file)?;
        if source.size() != saved.size || source.upload_source(cancelled)? != *saved {
            return Err(io::Error::other("The saved upload source changed"));
        }
        Ok(source)
    }
}
