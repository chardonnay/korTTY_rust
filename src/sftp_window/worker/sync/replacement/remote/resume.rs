//! A checkpoint can resume bytes in a private directory only after a new review.

use crate::sftp_window::worker::resume::persist::{
    Lease, ReplacementRecord, ReplacementReview, ReviewedRemote, Store, remote_components,
};

use super::*;

impl Recovery<'_> {
    pub(super) fn current(self) -> bool {
        durable::matches(self.control, self.snapshot)
    }
}

pub(super) struct Checkpoint<'a> {
    pub(super) recovery: Recovery<'a>,
    store: Store,
    record: ReplacementRecord,
    lease: Lease,
    saved: bool,
}

pub(super) fn stage<'a>(
    client: &mut Client,
    target: &Target<'_>,
    folder: &Folder,
    upload: &Upload<'_>,
    target_digest: [u8; 32],
    recovery: Option<Recovery<'a>>,
) -> Result<(RemoteStage, Option<Checkpoint<'a>>)> {
    let review = match recovery {
        Some(recovery) => bind(target, upload, target_digest, recovery)?,
        None => None,
    };
    require_digest(
        client,
        target.path,
        target.original,
        target_digest,
        &upload.stopped,
    )?;
    folder.verify(client, false)?;
    (upload.authorize)()?;
    let Some((review, recovery)) = review else {
        return Ok((RemoteStage::claim(client, target.folder, target.uid)?, None));
    };
    let store = Store::new(&recovery.snapshot.1.profile);
    let matching = store
        .replacements()?
        .into_iter()
        .find(|record| record.review == review);
    if let Some(record) = matching {
        let lease = store.lease_upload(&record.id, true)?;
        if !store.replacements()?.contains(&record) {
            return Err(io::Error::other("The saved replacement receipt changed").into());
        }
        let directory = record.directory()?;
        let mut checkpoint = Checkpoint {
            recovery,
            store,
            record,
            lease,
            saved: true,
        };
        if remote_existing(client, &directory)?.is_some() {
            let label = directory.escape_ascii().to_string();
            let result = RemoteStage::reopen(
                client,
                directory,
                target.uid,
                checkpoint.record.directory_group,
            );
            return match result {
                Ok(stage) => Ok((stage, Some(checkpoint))),
                Err(error) => {
                    if !error.disconnected {
                        checkpoint.retire(upload)?;
                    }
                    Err(error.context(format!(" · Check the remote staging path {label}")))
                }
            };
        }
        // An absent stage is safe to forget. No server entry is deleted here.
        checkpoint.retire(upload)?;
        return new_stage(client, target, upload, recovery, review, checkpoint.store);
    }
    new_stage(client, target, upload, recovery, review, store)
}

fn new_stage<'a>(
    client: &mut Client,
    target: &Target<'_>,
    upload: &Upload<'_>,
    recovery: Recovery<'a>,
    review: ReplacementReview,
    store: Store,
) -> Result<(RemoteStage, Option<Checkpoint<'a>>)> {
    let id = crate::private_files::nonce()?;
    let lease = store.lease_upload(&id, false)?;
    require_context(recovery, upload)?;
    let stage = RemoteStage::claim_named(
        client,
        target.folder,
        ReplacementRecord::name(&id).as_bytes(),
        target.uid,
    )?;
    let record = ReplacementRecord {
        id,
        review,
        directory_group: stage.gid,
        payload_group: stage.gid,
    };
    Ok((
        stage,
        Some(Checkpoint {
            recovery,
            store,
            record,
            lease,
            saved: false,
        }),
    ))
}

fn bind<'a>(
    target: &Target<'_>,
    upload: &Upload<'_>,
    target_digest: [u8; 32],
    recovery: Recovery<'a>,
) -> Result<Option<(ReplacementReview, Recovery<'a>)>> {
    let (_, DirectoryStamp::Remote(parent)) = target.parent else {
        return Ok(None);
    };
    let (Some(parent_gid), Some(parent_mode), Some(original)) = (
        parent.gid.filter(|&gid| gid != u32::MAX),
        parent.permissions,
        ReviewedRemote::capture(target.original, target_digest),
    ) else {
        return Ok(None);
    };
    if target.path.len() > 16_384
        || remote_components(target.path).is_none_or(|(parent, _)| parent != target.folder)
        || upload.file.size() > crate::sftp_window::worker::resume::MAX_PARTIAL_BYTES
    {
        return Ok(None);
    }
    require_context(recovery, upload)?;
    let source = upload
        .file
        .upload_source(&|| (upload.stopped)() || !recovery.current())?;
    let reviewed_sha256: String = upload
        .digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if source.sha256 != reviewed_sha256 {
        return Err(changed_bytes().into());
    }
    upload.verify()?;
    require_context(recovery, upload)?;
    let context = &recovery.snapshot.1;
    Ok(Some((
        ReplacementReview {
            route: context.route.clone(),
            host_key: context.host_key.to_openssh()?,
            remote: target.path.to_vec(),
            source,
            source_modified: upload.modified,
            original,
            parent_gid,
            parent_mode,
        },
        recovery,
    )))
}

fn require_context(recovery: Recovery<'_>, upload: &Upload<'_>) -> io::Result<()> {
    if !recovery.current() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    (upload.authorize)().map_err(|error| io::Error::other(error.message))
}

impl Checkpoint<'_> {
    pub(super) fn mode(&self) -> UploadStage {
        if self.saved {
            UploadStage::Resume(self.record.owner())
        } else {
            UploadStage::Create
        }
    }

    pub(super) fn record_owner(
        &mut self,
        owner: UploadOwner,
        upload: &Upload<'_>,
    ) -> io::Result<()> {
        require_context(self.recovery, upload)?;
        if self.saved {
            if owner != self.record.owner() {
                return Err(io::Error::other(
                    "The saved replacement stage owner changed",
                ));
            }
            return Ok(());
        }
        self.record.payload_group = owner.gid;
        // A directory sync error may follow successful publication of the receipt.
        self.lease.retain(true);
        let saved = self.store.save_replacement(self.record.clone(), &|| {
            require_context(self.recovery, upload).is_ok()
        });
        match saved {
            Ok(saved) => {
                self.saved = saved;
                self.lease.retain(saved);
                Ok(())
            }
            Err(error) => {
                if let Ok(records) = self.store.replacements() {
                    self.saved = records.contains(&self.record);
                    self.lease.retain(self.saved);
                }
                Err(error)
            }
        }
    }

    pub(super) fn retire(&mut self, upload: &Upload<'_>) -> Result<()> {
        if self.saved {
            self.store.remove_replacement(&self.record, &|| {
                require_context(self.recovery, upload).is_ok()
            })?;
            self.saved = false;
        }
        self.lease.retain(false);
        Ok(())
    }

    pub(super) fn on_preflight_failure(
        &mut self,
        failure: Failure,
        stage: &RemoteStage,
        upload: &Upload<'_>,
    ) -> Result<Failure> {
        let stopped = (upload.stopped)();
        let retain = self
            .recovery
            .control
            .resume_preferences()
            .retention(stopped);
        if self.saved && retain && (failure.disconnected || stopped || !self.recovery.current()) {
            return Err(failure.context(format!(
                " · Private replacement upload saved at {} · Confirm a fresh comparison to resume",
                stage.label()
            )));
        }
        let retired = if self.saved && !retain {
            let current = ClipboardPolicy::load();
            let result = self.store.remove_replacement(&self.record, &|| {
                durable::matches_context(self.recovery.control, self.recovery.snapshot)
                    && self.recovery.control.transfer_allowed(&current)
            });
            if result.is_ok() {
                self.saved = false;
                self.lease.retain(false);
            }
            result.map_err(Failure::from)
        } else {
            self.retire(upload)
        };
        retired.map_err(|error| {
            failure.clone().context(format!(
                " · Could not retire the replacement receipt ({}) · Check the remote staging path {}",
                error.message, stage.label()
            ))
        })?;
        Ok(failure)
    }
}
