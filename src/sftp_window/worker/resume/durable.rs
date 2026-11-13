//! Worker-side publication and recovery of private download receipts.

use super::persist::{Record, Store};
use super::*;

pub(in crate::sftp_window::worker) fn snapshot(control: &Control) -> Option<(u64, Context)> {
    if !control.resume_preferences().enabled {
        return None;
    }
    let current = control
        .resume
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    current.1.clone().map(|context| (current.0, context))
}

pub(in crate::sftp_window::worker) fn matches(
    control: &Control,
    snapshot: &(u64, Context),
) -> bool {
    !control.cancelled.load(Ordering::Acquire) && matches_context(control, snapshot)
}

pub(in crate::sftp_window::worker) fn matches_context(
    control: &Control,
    snapshot: &(u64, Context),
) -> bool {
    let current = control
        .resume
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    current.0 == snapshot.0 && current.1.as_ref() == Some(&snapshot.1)
}

pub(super) struct Prefix<'a>(
    pub(super) &'a std::fs::File,
    pub(super) &'a Control,
    pub(super) &'a Progress,
);

impl Read for Prefix<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.1.cancelled.load(Ordering::Acquire) || self.2.stopped() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.0.read(output)
    }
}

pub(in crate::sftp_window::worker) fn find(control: &Control) -> Result<Reply> {
    let snapshot = snapshot(control).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Saved transfers are unavailable in this window",
        )
    })?;
    let context = &snapshot.1;
    let records = Store::new(&context.profile).list()?;
    let mut skipped = 0;
    for record in records {
        if record.route != context.route
            || HostKey::from_openssh(&record.host_key)? != context.host_key
        {
            continue;
        }
        if !matches(control, &snapshot) {
            return Err(io::Error::from(io::ErrorKind::Interrupted).into());
        }
        if StagedDownload::checkpoint_published(&record.payload, record.total).unwrap_or(false)
            || StagedDownload::checkpoint_missing(&record.payload).unwrap_or(false)
        {
            Store::new(&context.profile).remove(&record.id)?;
            continue;
        }
        match Offer::recover(record, context, || !matches(control, &snapshot)) {
            Ok(offer) => {
                return Ok(Reply::InterruptedDownload {
                    offer: Some(offer),
                    note: "Recovered a verified partial download. Review it before resuming."
                        .into(),
                });
            }
            Err(_) => skipped += 1,
        }
    }
    let note = if skipped == 0 {
        "No interrupted download matches this authenticated connection.".into()
    } else {
        format!("No resumable download found. {skipped} saved transfer(s) could not be verified.")
    };
    Ok(Reply::InterruptedDownload { offer: None, note })
}

pub(in crate::sftp_window::worker) fn find_transfer(
    client: &mut Client,
    control: &Control,
) -> Result<Reply> {
    let download = find(control)?;
    if matches!(&download, Reply::InterruptedDownload { offer: Some(_), .. }) {
        return Ok(download);
    }
    let snapshot = snapshot(control).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Saved transfers are unavailable in this window",
        )
    })?;
    let uploads = super::upload::find(client, control, &snapshot)?;
    if let Some(offer) = uploads.offer {
        return Ok(Reply::InterruptedDownload {
            offer: Some(offer),
            note: "Recovered a verified partial upload. Review it before resuming.".into(),
        });
    }
    let host_key = snapshot.1.host_key.to_openssh()?;
    let replacements = Store::new(&snapshot.1.profile)
        .replacements()?
        .into_iter()
        .filter(|record| {
            record.review.route == snapshot.1.route && record.review.host_key == host_key
        })
        .count();
    if replacements != 0 {
        return Ok(Reply::InterruptedDownload {
            offer: None,
            note: format!(
                "{replacements} saved replacement upload(s). Confirm a fresh synchronization review of the same files to resume."
            ),
        });
    }
    if uploads.completed != 0 || uploads.discarded != 0 || uploads.skipped != 0 {
        return Ok(Reply::InterruptedDownload {
            offer: None,
            note: format!(
                "Saved uploads checked · {} already published · {} discarded · {} could not be verified.",
                uploads.completed, uploads.discarded, uploads.skipped
            ),
        });
    }
    Ok(download)
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::sftp_window) struct Context {
    pub(in crate::sftp_window) profile: PathBuf,
    pub(in crate::sftp_window) route: crate::workspace::SshTarget,
    pub(in crate::sftp_window) host_key: HostKey,
}

pub(super) struct Receipt {
    profile: PathBuf,
    id: String,
}

impl Receipt {
    pub(super) fn remove(&self) -> io::Result<()> {
        Store::new(&self.profile).remove(&self.id)
    }
}

impl Offer {
    pub(super) fn persist_checkpoint(
        &self,
        context: &Context,
        allowed: impl Fn() -> bool,
    ) -> io::Result<()> {
        if !self.matches_peer(Some(&context.host_key)) || !allowed() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let state = self
            .download_state()
            .ok_or_else(|| io::Error::other("No partial download was selected"))?;
        let mut current = state.lock().unwrap_or_else(|error| error.into_inner());
        let download = current
            .as_mut()
            .ok_or_else(|| io::Error::other("The partial download is no longer available"))?;
        let checkpoint = download
            .checkpoint
            .as_ref()
            .ok_or_else(|| io::Error::other("The partial download has no checked prefix"))?;
        download.staged.lock_checkpoint()?;
        download.staged.verify_partial(&checkpoint.stamp)?;
        let payload = download.staged.checkpoint(|| !allowed())?;
        let id = payload
            .stage_name
            .strip_prefix(".kortty-")
            .and_then(|name| name.strip_suffix(".kortty-part"))
            .ok_or_else(|| io::Error::other("Invalid private download stage"))?
            .to_owned();
        let record = Record {
            id: id.clone(),
            route: context.route.clone(),
            host_key: context.host_key.to_openssh()?,
            remote: download.source.clone(),
            total: self.total,
            remote_modified: download
                .attributes
                .modified
                .ok_or_else(|| io::Error::other("Missing remote timestamp"))?,
            remote_permissions: download
                .attributes
                .permissions
                .ok_or_else(|| io::Error::other("Missing remote file mode"))?,
            remote_uid: download.attributes.uid,
            remote_gid: download.attributes.gid,
            title: download.title.clone(),
            payload,
        };
        let store = Store::new(&context.profile);
        store.save_guarded(record, &allowed)?;
        if !allowed() {
            store.remove(&id)?;
            return Err(io::ErrorKind::Interrupted.into());
        }
        download.staged.keep_checkpoint();
        download.receipt = Some(Receipt {
            profile: context.profile.clone(),
            id,
        });
        Ok(())
    }

    pub(super) fn recover(
        record: Record,
        context: &Context,
        cancelled: impl Fn() -> bool,
    ) -> io::Result<Self> {
        if record.route != context.route
            || HostKey::from_openssh(&record.host_key)? != context.host_key
        {
            return Err(io::Error::other(
                "The saved transfer belongs to another authenticated route",
            ));
        }
        let target = record.payload.destination.path()?;
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Invalid saved download target"))?
            .to_path_buf();
        let staged = StagedDownload::reopen_checkpoint(&record.payload, cancelled)?;
        let stamp = FileStamp::read(staged.writer()?)?;
        let offset = record.payload.offset;
        let total = record.total;
        let download = Download {
            source: record.remote,
            target,
            target_directory: Location::Local(parent),
            title: record.title,
            attributes: Attributes {
                size: Some(record.total),
                uid: record.remote_uid,
                gid: record.remote_gid,
                permissions: Some(record.remote_permissions),
                modified: Some(record.remote_modified),
                accessed: None,
            },
            staged,
            checkpoint: Some(Checkpoint { offset, stamp }),
            receipt: Some(Receipt {
                profile: context.profile.clone(),
                id: record.id,
            }),
        };
        Ok(Self::new(download, context.host_key.clone(), offset, total))
    }
}

impl Download {
    pub(super) fn discard_receipt(&mut self) -> io::Result<()> {
        if let Some(receipt) = &self.receipt {
            receipt.remove()?;
        }
        self.receipt = None;
        Ok(())
    }
}
