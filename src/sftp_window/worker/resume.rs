//! A private, handle-bound download checkpoint for an interrupted SFTP transfer.

use std::fmt;
use std::io::{Seek, SeekFrom};

use kortty_ssh::sftp::DOWNLOAD_OVERLAP_BYTES;

use crate::private_files::FileStamp;
use crate::sftp_window::StagedDownload;

use super::*;

pub(in crate::sftp_window::worker) const MAX_PARTIAL_BYTES: u64 = 1024 * 1024 * 1024;

mod discard;
pub(super) mod durable;
pub(in crate::sftp_window) mod persist;
pub(super) mod upload;

struct Checkpoint {
    offset: u64,
    stamp: FileStamp,
}

struct Download {
    source: Vec<u8>,
    target: PathBuf,
    target_directory: Location,
    title: String,
    attributes: Attributes,
    staged: StagedDownload,
    checkpoint: Option<Checkpoint>,
    receipt: Option<durable::Receipt>,
}

#[derive(Clone)]
pub(crate) struct Offer {
    state: OfferState,
    host_key: HostKey,
    route: String,
    offset: u64,
    total: u64,
}

#[derive(Clone)]
enum OfferState {
    Download(Arc<Mutex<Option<Download>>>),
    Upload(Arc<Mutex<Option<upload::Saved>>>),
}

impl fmt::Debug for Offer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SftpResume")
            .field("offset", &self.offset)
            .finish_non_exhaustive()
    }
}

impl Offer {
    fn new(download: Download, host_key: HostKey, offset: u64, total: u64) -> Self {
        Self {
            route: format!(
                "{} → {}",
                files::display(&String::from_utf8_lossy(&download.source)),
                files::display(&download.target.to_string_lossy()),
            ),
            state: OfferState::Download(Arc::new(Mutex::new(Some(download)))),
            host_key,
            offset,
            total,
        }
    }

    fn upload(saved: upload::Saved, host_key: &HostKey, offset: u64) -> Self {
        let source = saved.record.source.path.path().map_or_else(
            |_| "<local file>".into(),
            |path| files::display(&path.to_string_lossy()),
        );
        let remote = files::display(&String::from_utf8_lossy(&saved.record.remote));
        let total = saved.record.source.size;
        Self {
            state: OfferState::Upload(Arc::new(Mutex::new(Some(saved)))),
            host_key: host_key.clone(),
            route: format!("{source} → {remote}"),
            offset,
            total,
        }
    }

    pub(in crate::sftp_window) fn is_upload(&self) -> bool {
        matches!(&self.state, OfferState::Upload(_))
    }

    fn download_state(&self) -> Option<&Mutex<Option<Download>>> {
        match &self.state {
            OfferState::Download(state) => Some(state),
            OfferState::Upload(_) => None,
        }
    }

    pub(in crate::sftp_window) fn matches_peer(&self, key: Option<&HostKey>) -> bool {
        key == Some(&self.host_key)
    }

    pub(in crate::sftp_window) fn label(&self) -> String {
        format!(
            "Partial {} · {} · {}/{} bytes",
            if self.is_upload() {
                "upload"
            } else {
                "download"
            },
            self.route,
            self.offset,
            self.total
        )
    }

    pub(in crate::sftp_window) fn progress(&self) -> Progress {
        Progress::new(Some(self.total))
    }

    pub(in crate::sftp_window) fn available(&self) -> bool {
        match &self.state {
            OfferState::Download(state) => state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some(),
            OfferState::Upload(state) => state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_some(),
        }
    }

    pub(in crate::sftp_window) fn discard(self) {
        let _ = self.try_discard();
    }

    fn take(&self) -> Option<Download> {
        self.download_state()?
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    pub(super) fn run(
        self,
        client: &mut Client,
        progress: &Progress,
        control: &Control,
    ) -> Result<Reply> {
        if let OfferState::Upload(state) = &self.state {
            let upload = state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .ok_or_else(|| io::Error::other("The partial upload is no longer available"))?;
            return upload.run(client, progress, control);
        }
        let download = self
            .take()
            .ok_or_else(|| io::Error::other("The partial download is no longer available"))?;
        if !self.matches_peer(control.host_key.as_ref()) {
            download.discard();
            return Err(io::Error::other(
                "The verified SSH host key changed. Start a new download.",
            )
            .into());
        }
        let directory = download.target_directory.clone();
        let title = download.title.clone();
        let count = download.copy(client, progress, control)?;
        changed(
            client,
            &directory,
            format!("Resumed {title} · {count} bytes"),
        )
    }

    pub(super) fn discard_upload(self, client: &mut Client, control: &Control) -> Result<Reply> {
        let OfferState::Upload(state) = &self.state else {
            return Err(io::Error::other("No remote upload was selected").into());
        };
        let upload = state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| io::Error::other("The partial upload is no longer available"))?;
        upload.discard(client, control)?;
        drop(
            state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        );
        Ok(Reply::InterruptedDownload {
            offer: None,
            note: "Partial upload discarded.".into(),
        })
    }
}

pub(super) fn download(
    client: &mut Client,
    transfer: &Transfer,
    source: &[u8],
    target: &Path,
    progress: &Progress,
    control: &Control,
) -> Result<u64> {
    let attributes = client.lstat(source)?;
    if attributes.kind() != FileKind::File {
        return Err(io::Error::other("Download source is no longer a regular file").into());
    }
    let download = Download {
        source: source.to_vec(),
        target: target.to_path_buf(),
        target_directory: transfer.target_directory.clone(),
        title: transfer.title.clone(),
        attributes,
        staged: StagedDownload::resumable(target)?,
        checkpoint: None,
        receipt: None,
    };
    download.copy(client, progress, control)
}

impl Download {
    fn copy(mut self, client: &mut Client, progress: &Progress, control: &Control) -> Result<u64> {
        let persistence = durable::snapshot(control);
        // Reopened checkpoints retain their private payload. Final access is
        // derived afresh from the actual destination parent for this attempt.
        let access = match self.download_access() {
            Ok(access) => access,
            Err(error) => {
                self.discard();
                return Err(error.into());
            }
        };
        let count = match self.copy_bytes(client, progress, control) {
            Ok(count) => count,
            Err(error) => {
                let cancelled = progress.stopped();
                let retain = || {
                    control.resume_preferences().retention(cancelled)
                        && (!control.cancelled.load(Ordering::Acquire) || cancelled)
                        && control.transfer_allowed(&ClipboardPolicy::load())
                };
                let eligible = (error.retain_download || cancelled) && retain();
                if eligible && let Some(key) = &control.host_key {
                    let mut failure = error;
                    match self.checkpoint(key.clone()) {
                        Ok(offer) => {
                            if let Some(snapshot) = persistence
                                && let Err(error) = offer.persist_checkpoint(&snapshot.1, || {
                                    retain() && durable::matches_context(control, &snapshot)
                                })
                            {
                                failure.message.push_str(&format!(
                                    " · Kept in this application only ({error})"
                                ));
                            }
                            failure.partial = Some(offer);
                        }
                        Err(error) => failure.message.push_str(&format!(
                            " · Could not retain the partial download ({error})"
                        )),
                    }
                    return Err(failure);
                }
                self.discard();
                return Err(error);
            }
        };
        if let Err(error) = self
            .staged
            .verify_partial(&FileStamp::read(self.staged.writer()?)?)
        {
            self.discard();
            return Err(error.into());
        }
        let receipt = self.receipt.take();
        self.staged.publish_checked(|file| {
            progress.check_cancelled()?;
            if control.is_closed() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            access.apply_to(file)?;
            file.sync_all()?;
            access.verify_parent()?;
            progress.check_cancelled()?;
            if control.is_closed() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok(())
        })?;
        if let Some(receipt) = receipt {
            receipt.remove()?;
        }
        Ok(count)
    }

    fn download_access(&self) -> io::Result<download_access::Access> {
        let parent = self
            .target
            .parent()
            .ok_or_else(|| io::Error::other("The download destination has no parent"))?;
        download_access::Access::capture(
            parent,
            &self.staged.parent_identity()?,
            self.attributes.permissions,
        )
    }

    fn copy_bytes(
        &self,
        client: &mut Client,
        progress: &Progress,
        control: &Control,
    ) -> Result<u64> {
        let mut file = self.staged.writer()?;
        let (offset, overlap) = if let Some(checkpoint) = &self.checkpoint {
            self.staged.verify_partial(&checkpoint.stamp)?;
            let offset = checkpoint.offset;
            let mut overlap = vec![0; offset.min(DOWNLOAD_OVERLAP_BYTES as u64) as usize];
            file.seek(SeekFrom::Start(offset - overlap.len() as u64))?;
            file.read_exact(&mut overlap)?;
            (offset, overlap)
        } else {
            (0, Vec::new())
        };
        progress.set_position(offset)?;
        if control.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::from(io::ErrorKind::Interrupted).into());
        }
        let count = if self.receipt.is_some() {
            file.seek(SeekFrom::Start(0))?;
            client.download_regular_with_prefix(
                &self.source,
                self.attributes,
                offset,
                durable::Prefix(file, control, progress),
                Counted(progress, file),
            )?
        } else {
            client.download_regular_from(
                &self.source,
                self.attributes,
                offset,
                &overlap,
                Counted(progress, file),
            )?
        };
        if self.staged.writer()?.metadata()?.len() != count {
            return Err(io::Error::other("The local download changed during transfer").into());
        }
        Ok(count)
    }

    fn discard(mut self) {
        if self.discard_receipt().is_ok() {
            self.staged.discard_checkpoint();
        }
    }

    fn checkpoint(mut self, host_key: HostKey) -> io::Result<Offer> {
        let file = self.staged.writer()?;
        let offset = file.metadata()?.len();
        let total = self
            .attributes
            .size
            .filter(|total| {
                self.attributes.modified.is_some()
                    && offset > 0
                    && offset <= *total
                    && offset <= MAX_PARTIAL_BYTES
            })
            .ok_or_else(|| io::Error::other("The interrupted download cannot be resumed"))?;
        file.sync_all()?;
        let stamp = FileStamp::read(file)?;
        self.staged.verify_partial(&stamp)?;
        self.checkpoint = Some(Checkpoint { offset, stamp });
        Ok(Offer::new(self, host_key, offset, total))
    }
}

#[cfg(test)]
mod tests;
