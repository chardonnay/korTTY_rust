//! Stages a reviewed server replacement and publishes into the vacant name.

use std::cell::RefCell;

use kortty_ssh::sftp::{StatusCode, UploadMetadata, UploadStage};

use crate::sftp_window::worker::server_copy::Folder;

use super::*;

mod in_place;
mod reservation;
mod resume;

struct Target<'a> {
    path: &'a [u8],
    folder: &'a [u8],
    parent: &'a (Location, DirectoryStamp),
    original: Attributes,
    uid: u32,
}

impl<'a> Target<'a> {
    fn from_entry(entry: &'a Entry) -> Result<Self> {
        let (
            Location::Remote(path),
            SnapshotEntry::Remote(original),
            (Location::Remote(folder), DirectoryStamp::Remote(parent)),
        ) = (&entry.target, &entry.target_stamp, &entry.target_parent)
        else {
            return Err(io::Error::other("The reviewed server destination is invalid").into());
        };
        let uid = require_private_parent(*parent)?;
        let name = path.rsplit(|&byte| byte == b'/').next().unwrap_or_default();
        files::validate_transfer_name(name)?;
        if files::remote_join(folder, name) != *path
            || original.kind() != FileKind::File
            || original.uid.is_none_or(|value| value == u32::MAX)
            || original.gid.is_none_or(|value| value == u32::MAX)
            || original.permissions.is_none()
        {
            return Err(io::Error::other(
                "Reviewed server replacement requires a regular file with known ownership and permissions",
            )
            .into());
        }
        Ok(Self {
            path,
            folder,
            parent: &entry.target_parent,
            original: *original,
            uid,
        })
    }

    fn in_place(&self) -> bool {
        self.original.uid != Some(self.uid)
    }

    fn checked_folder(&self, client: &mut Client, stopped: &impl Fn() -> bool) -> Result<Folder> {
        if self.in_place() {
            if self
                .original
                .permissions
                .is_none_or(|mode| mode & 0o7000 != 0)
            {
                return Err(io::Error::other(
                    "In-place replacement is unavailable for a file with special permission bits",
                )
                .into());
            }
        } else if !client.supports_atomic_replace() || !client.supports_hardlink() {
            return Err(io::Error::other(
                "Reviewed server replacement requires the SFTP POSIX rename and hard-link extensions",
            )
            .into());
        }
        let (location, stamp) = self.parent;
        stamp.verify(client, location, false)?;
        let folder = Folder::read(client, self.folder.to_vec(), stopped)?;
        if folder.owner() != self.uid || !folder.trusted(self.uid) {
            return Err(io::Error::other("The server destination folder is not protected").into());
        }
        Ok(folder)
    }
}

pub(super) fn in_place(entry: &Entry) -> bool {
    Target::from_entry(entry).is_ok_and(|target| target.in_place())
}

pub(super) fn prepare(
    client: &mut Client,
    entry: &Entry,
    stopped: &impl Fn() -> bool,
) -> Result<()> {
    let target = Target::from_entry(entry)?;
    target.checked_folder(client, stopped)?;
    upload_time(entry)?;
    Ok(())
}

pub(super) fn replace(
    client: &mut Client,
    entry: &Entry,
    digests: [[u8; 32]; 2],
    progress: &Progress,
    stopped: &impl Fn() -> bool,
    authorize: &impl Fn() -> Result<()>,
    scope: ReplaceScope<'_>,
) -> ReplaceResult {
    let target = Target::from_entry(entry)?;
    let folder = target.checked_folder(client, stopped)?;
    let Location::Local(path) = &entry.source else {
        return Err(io::Error::other("The reviewed local source is invalid").into());
    };
    let upload = Upload {
        entry,
        file: LocalSource::open(path)?,
        progress,
        modified: upload_time(entry)?,
        digest: digests[0],
        stopped,
        authorize,
    };
    upload.verify()?;
    authorize()?;
    let _reserved = reservation::Reservation::claim(scope, target.path)?;
    if target.in_place() {
        return in_place::replace(client, &target, &folder, &upload, digests[1]);
    }
    let (stage, mut checkpoint) = resume::stage(
        client,
        &target,
        &folder,
        &upload,
        digests[1],
        scope.recovery,
    )?;
    let mut state = StageState::default();
    if let Err(error) = upload.write_stage(client, &stage, &mut state, checkpoint.as_mut()) {
        return Err(with_stage_recovery(
            error,
            client,
            &folder,
            &stage,
            &upload,
            &state,
            checkpoint.as_mut(),
        )
        .into());
    }
    let group_warning = match upload.finalize_stage(
        client,
        &stage,
        target.original,
        &mut state,
        checkpoint.as_mut(),
    ) {
        Ok(warning) => warning,
        Err(error) => {
            return Err(with_stage_recovery(
                error,
                client,
                &folder,
                &stage,
                &upload,
                &state,
                checkpoint.as_mut(),
            )
            .into());
        }
    };
    let stage_stamp = state.verified.expect("completed verified upload stage");
    let backup = match RemoteStage::claim(client, target.folder, target.uid) {
        Ok(backup) => backup,
        Err(error) => {
            return Err(with_stage_recovery(
                error,
                client,
                &folder,
                &stage,
                &upload,
                &state,
                checkpoint.as_mut(),
            )
            .into());
        }
    };
    let backup_label = backup.payload.escape_ascii().to_string();
    let recovery = || {
        format!(
            " · Original recovery path {backup_label} · Staging path {}",
            stage.label()
        )
    };
    let before_move = (|| -> Result<()> {
        folder.verify(client, false)?;
        require_digest(client, target.path, target.original, digests[1], stopped)?;
        folder.verify(client, false)?;
        backup.verify(client)?;
        require_absent(client, &backup.payload)?;
        stage.verify(client)?;
        require_entry(client, &stage.payload, stage_stamp)?;
        upload.verify()?;
        authorize()?;
        // Metadata may already match. In that case quarantine is the first
        // boundary at which this complete private stage must stop being resumable.
        if let Some(checkpoint) = &mut checkpoint {
            checkpoint.retire(&upload)?;
            folder.verify(client, false)?;
            backup.verify(client)?;
            stage.verify(client)?;
            require_entry(client, &stage.payload, stage_stamp)?;
        }
        // Retiring the receipt can take local I/O. Keep the final name checks
        // after it, directly before the unconditional quarantine rename.
        require_absent(client, &backup.payload)?;
        require_entry(client, target.path, target.original)?;
        authorize()?;
        Ok(())
    })();
    if let Err(error) = before_move {
        let error = if let Some(checkpoint) = &mut checkpoint {
            checkpoint
                .on_preflight_failure(error, &stage, &upload)
                .map_err(|error| {
                    error.context(format!(
                        " · Check the private backup path {}",
                        backup.label()
                    ))
                })?
        } else {
            error
        };
        return Err(
            clean_before_move(error, client, &folder, &stage, &backup, &upload, &state).into(),
        );
    }
    if let Err(error) = client.replace(target.path, &backup.payload) {
        let error = Failure::from(error);
        if error.disconnected {
            return Err(ReplaceFailure::uncertain(
                error
                    .context(" · Server quarantine could not be confirmed")
                    .context(recovery()),
            ));
        }
        return Err(
            clean_before_move(error, client, &folder, &stage, &backup, &upload, &state).into(),
        );
    }

    let mut published = false;
    let result = (|| -> Result<()> {
        backup.verify(client)?;
        require_digest(
            client,
            &backup.payload,
            target.original,
            digests[1],
            stopped,
        )?;
        folder.verify(client, false)?;
        stage.verify(client)?;
        require_entry(client, &stage.payload, stage_stamp)?;
        upload.verify()?;
        authorize()?;
        client.hardlink(&stage.payload, target.path)?;
        published = true;
        require_entry(client, target.path, stage_stamp)
    })();
    if let Err(error) = result {
        if published || error.disconnected {
            return Err(ReplaceFailure::uncertain(
                error
                    .context(" · Server publication could not be confirmed")
                    .context(recovery()),
            ));
        }
        match restore(client, &folder, &backup, &target) {
            Ok(()) => {
                let error = error.context(format!(
                    " · The displaced file was restored · Original recovery path {backup_label}"
                ));
                return Err(
                    with_stage_cleanup(error, client, &folder, &stage, &upload, &state).into(),
                );
            }
            Err(restore) => {
                let disconnected = restore.disconnected;
                let mut error = error
                    .context(format!(" · Restore failed ({})", restore.message))
                    .context(recovery());
                error.disconnected |= disconnected;
                return Err(ReplaceFailure::uncertain(error));
            }
        }
    }
    let mut warning = clean_stage(client, &folder, &stage, &upload, &state)
        .err()
        .map(|error| {
            error.context(format!(
                " · Check the remote staging path {}",
                stage.label()
            ))
        });
    if let Some(group_warning) = group_warning {
        warning = Some(warning.map_or(group_warning.clone(), |error| {
            error.context(format!(" · {}", group_warning.message))
        }));
    }
    Ok(Completion {
        backup: backup_label,
        warning,
    })
}

#[derive(Default)]
struct StageState {
    owner: Option<UploadOwner>,
    verified: Option<Attributes>,
}

struct Upload<'a> {
    entry: &'a Entry,
    file: LocalSource,
    progress: &'a Progress,
    modified: u32,
    digest: [u8; 32],
    stopped: &'a dyn Fn() -> bool,
    authorize: &'a dyn Fn() -> Result<()>,
}

impl Upload<'_> {
    fn verify(&self) -> Result<()> {
        let (SnapshotEntry::LocalFile { stamp, identity }, (_, DirectoryStamp::Local(parent))) =
            (&self.entry.source_stamp, &self.entry.source_parent)
        else {
            return Err(io::Error::other("The reviewed local source is invalid").into());
        };
        Ok(self.file.verify_reviewed(stamp, identity, parent)?)
    }

    fn write_stage(
        &self,
        client: &mut Client,
        stage: &RemoteStage,
        state: &mut StageState,
        mut checkpoint: Option<&mut resume::Checkpoint<'_>>,
    ) -> Result<()> {
        stage.verify(client)?;
        (self.authorize)()?;
        let mode = checkpoint
            .as_ref()
            .map_or(UploadStage::Create, |value| value.mode());
        let recovery = checkpoint.as_ref().map(|value| value.recovery);
        let base = self.progress.bytes.load(Ordering::Relaxed);
        client.upload_private_verified(
            &stage.payload,
            mode,
            self.file.size(),
            Counted(self.progress, self.file.file()),
            |owner, offset| {
                if owner.uid != stage.uid {
                    return Err(io::Error::other("The private stage has a different owner"));
                }
                state.owner = Some(owner);
                self.verify()
                    .map_err(|error| io::Error::other(error.message))?;
                (self.authorize)().map_err(|error| io::Error::other(error.message))?;
                if let Some(checkpoint) = &mut checkpoint {
                    checkpoint.record_owner(owner, self)?;
                }
                self.progress.set_position(base + offset)
            },
            || (self.stopped)() || recovery.is_some_and(|value| !value.current()),
        )?;
        Ok(())
    }

    fn finalize_stage(
        &self,
        client: &mut Client,
        stage: &RemoteStage,
        target: Attributes,
        state: &mut StageState,
        mut checkpoint: Option<&mut resume::Checkpoint<'_>>,
    ) -> Result<Option<Failure>> {
        let owner = state
            .owner
            .ok_or_else(|| io::Error::other("The upload owner was not observed"))?;
        let initial = client.lstat(&stage.payload)?;
        if initial.kind() != FileKind::File
            || initial.uid != Some(owner.uid)
            || initial.gid != Some(owner.gid)
            || initial.size != Some(self.file.size())
            || initial
                .permissions
                .is_none_or(|mode| mode & 0o7777 != 0o600)
        {
            return Err(io::Error::other("The staged upload changed after writing").into());
        }
        state.verified = Some(initial);
        let verified = state.verified.as_mut().expect("observed private upload");
        let mode = target.permissions.map(|mode| mode & 0o7777);
        let wanted = UploadMetadata {
            uid: None,
            gid: target.gid,
            mode,
        };
        let group_warning =
            match self.metadata(client, &stage.payload, verified, wanted, &mut checkpoint) {
                Ok(()) => None,
                Err(Error::Status(status))
                    if matches!(
                        status.code,
                        StatusCode::PermissionDenied | StatusCode::Failure
                    ) && wanted.gid != initial.gid
                        && unchanged(initial, *verified)
                        && unchanged(initial, client.lstat(&stage.payload)?) =>
                {
                    let fallback = UploadMetadata {
                        gid: None,
                        ..wanted
                    };
                    self.metadata(client, &stage.payload, verified, fallback, &mut checkpoint)?;
                    Some(io::Error::other("The server retained the staging file's group").into())
                }
                Err(error) => return Err(error.into()),
            };
        self.verify()?;
        Ok(group_warning)
    }

    fn metadata(
        &self,
        client: &mut Client,
        path: &[u8],
        verified: &mut Attributes,
        metadata: UploadMetadata,
        checkpoint: &mut Option<&mut resume::Checkpoint<'_>>,
    ) -> std::result::Result<(), Error> {
        let digest = RefCell::new(Some(
            HashAlgorithm::Sha256
                .new_hasher()
                .map_err(|error| Error::LocalIo(io::Error::other(error)))?,
        ));
        let input = ObserveInput(self.file.file(), |bytes: &[u8]| {
            if let Some(digest) = digest.borrow_mut().as_mut() {
                digest.update(bytes);
            }
        });
        let verify_digest = || -> io::Result<()> {
            if let Some(digest) = digest.borrow_mut().take()
                && hash_bytes(digest.finalize()) != self.digest
            {
                return Err(changed_bytes());
            }
            Ok(())
        };
        client.finalize_private_upload(
            path,
            verified,
            input,
            (metadata, Some(self.modified)),
            self.stopped,
            || {
                verify_digest()?;
                if let Some(checkpoint) = checkpoint.as_deref_mut() {
                    checkpoint
                        .retire(self)
                        .map_err(|error| io::Error::other(error.message))?;
                }
                self.verify()
                    .map_err(|error| io::Error::other(error.message))?;
                (self.authorize)().map_err(|error| io::Error::other(error.message))
            },
        )?;
        verify_digest().map_err(Error::LocalIo)
    }
}

fn upload_time(entry: &Entry) -> Result<u32> {
    let (_, modified) = entry
        .source_stamp
        .file_metadata()
        .ok_or_else(|| io::Error::other("The reviewed local modification time is unavailable"))?;
    Ok(wire_modified(modified)?)
}

fn require_entry(client: &mut Client, path: &[u8], expected: Attributes) -> Result<()> {
    if client.canonicalize(path)? != path || !unchanged(expected, client.lstat(path)?) {
        return Err(io::Error::other("The reviewed server file changed").into());
    }
    Ok(())
}

fn require_digest(
    client: &mut Client,
    path: &[u8],
    stamp: Attributes,
    expected: [u8; 32],
    stopped: &impl Fn() -> bool,
) -> Result<()> {
    require_entry(client, path, stamp)?;
    let mut digest = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let sink = ObserveOutput(io::sink(), |bytes: &[u8]| digest.update(bytes));
    client.download_regular(path, stamp, CheckedOutput(sink, stopped))?;
    if hash_bytes(digest.finalize()) != expected {
        return Err(io::Error::other("The reviewed server destination bytes changed").into());
    }
    require_entry(client, path, stamp)
}

fn restore(
    client: &mut Client,
    folder: &Folder,
    backup: &RemoteStage,
    target: &Target<'_>,
) -> Result<()> {
    folder.verify(client, false)?;
    backup.verify(client)?;
    let displaced = client.lstat(&backup.payload)?;
    if displaced.kind() != FileKind::File || displaced.uid != Some(target.uid) {
        return Err(io::Error::other("The displaced entry cannot be safely restored").into());
    }
    require_entry(client, &backup.payload, displaced)?;
    client.hardlink(&backup.payload, target.path)?;
    require_entry(client, target.path, displaced)
}

fn clean_stage(
    client: &mut Client,
    folder: &Folder,
    stage: &RemoteStage,
    upload: &Upload<'_>,
    state: &StageState,
) -> Result<()> {
    folder.verify(client, false)?;
    stage.verify(client)?;
    if remote_existing(client, &stage.payload)?.is_none() {
        client.remove_dir(&stage.directory)?;
        return Ok(());
    }
    if let Some(stamp) = state.verified {
        require_entry(client, &stage.payload, stamp)?;
    } else {
        let owner = state
            .owner
            .ok_or_else(|| io::Error::other("The private stage owner is unavailable"))?;
        upload.verify()?;
        client.verify_private_upload(
            &stage.payload,
            owner,
            upload.file.size(),
            upload.file.file(),
            || false,
        )?;
        upload.verify()?;
    }
    stage.remove(client)
}

fn with_stage_cleanup(
    mut error: Failure,
    client: &mut Client,
    folder: &Folder,
    stage: &RemoteStage,
    upload: &Upload<'_>,
    state: &StageState,
) -> Failure {
    if !error.disconnected {
        match clean_stage(client, folder, stage, upload, state) {
            Ok(()) => return error,
            Err(cleanup) => {
                error.disconnected |= cleanup.disconnected;
                error = error.context(format!(" · Stage cleanup failed ({})", cleanup.message));
            }
        }
    }
    error.context(format!(
        " · Check the remote staging path {}",
        stage.label()
    ))
}

fn with_stage_recovery(
    error: Failure,
    client: &mut Client,
    folder: &Folder,
    stage: &RemoteStage,
    upload: &Upload<'_>,
    state: &StageState,
    checkpoint: Option<&mut resume::Checkpoint<'_>>,
) -> Failure {
    let error = if let Some(checkpoint) = checkpoint {
        match checkpoint.on_preflight_failure(error, stage, upload) {
            Ok(error) => error,
            Err(error) => return error,
        }
    } else {
        error
    };
    with_stage_cleanup(error, client, folder, stage, upload, state)
}

fn clean_before_move(
    mut error: Failure,
    client: &mut Client,
    folder: &Folder,
    stage: &RemoteStage,
    backup: &RemoteStage,
    upload: &Upload<'_>,
    state: &StageState,
) -> Failure {
    if error.disconnected {
        error = error.context(format!(
            " · Check the private backup path {}",
            backup.label()
        ));
    } else {
        let empty = (|| -> Result<()> {
            folder.verify(client, false)?;
            backup.verify(client)?;
            client.remove_dir(&backup.directory)?;
            Ok(())
        })();
        if let Err(cleanup) = empty {
            error.disconnected |= cleanup.disconnected;
            error = error.context(format!(
                " · Backup cleanup failed ({}) · Check the private backup path {}",
                cleanup.message,
                backup.label()
            ));
        }
    }
    with_stage_cleanup(error, client, folder, stage, upload, state)
}
