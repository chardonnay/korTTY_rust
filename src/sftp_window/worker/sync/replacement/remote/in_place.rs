//! Saves and verifies a private original before updating a foreign-owned file.

use std::cell::Cell;

use kortty_ssh::sftp::{InPlaceUpload, UploadStage};

use super::*;

pub(super) fn replace(
    client: &mut Client,
    target: &Target<'_>,
    folder: &Folder,
    upload: &Upload<'_>,
    original_digest: [u8; 32],
) -> ReplaceResult {
    let original_size = target
        .original
        .size
        .ok_or_else(|| io::Error::other("The reviewed server destination size is unavailable"))?;
    let original = SealedFile::new(original_size, original_digest, upload.stopped, |output| {
        Ok(client.download_regular(target.path, target.original, output)?)
    })?;
    let replacement = SealedFile::new(
        upload.file.size(),
        upload.digest,
        upload.stopped,
        |output| {
            let mut source = upload.file.file();
            source.rewind()?;
            Ok(io::copy(&mut source.take(upload.file.size() + 1), output)?)
        },
    )?;
    upload.verify()?;
    folder.verify(client, false)?;
    (upload.authorize)()?;
    // Claiming a 0700 stage verifies the login's reported owner before the
    // existing file can be opened for writing.
    let backup = RemoteStage::claim(client, target.folder, target.uid)?;
    let backup_path = backup.payload.escape_ascii().to_string();
    let backup_stamp = save_backup(client, &backup, &original, original_size, upload)
        .map_err(|error| error.context(format!(
            " · The original destination was not written · Check the unverified private backup path {backup_path}"
        )))?;

    let preserved = || format!(" · Original saved at {backup_path}");
    let result = (|| -> Result<()> {
        folder.verify(client, false)?;
        backup.verify(client)?;
        require_entry(client, &backup.payload, backup_stamp)?;
        upload.verify()?;
        original.verify()?;
        replacement.verify()?;
        (upload.authorize)()
    })();
    result.map_err(|error| error.context(preserved()))?;

    let progress_at_start = upload.progress.bytes.load(Ordering::Relaxed);
    let attempted = Cell::new(false);
    let acknowledged = Cell::new(0);
    let result = client.upload_in_place(
        target.path,
        InPlaceUpload {
            original: target.original,
            size: upload.file.size(),
            modified: upload.modified,
        },
        original.file()?,
        replacement.file()?,
        upload.stopped,
        |bytes| {
            acknowledged.set(bytes);
            upload.progress.set_position(progress_at_start + bytes)?;
            (upload.authorize)().map_err(|error| io::Error::other(error.message))?;
            original.verify()?;
            replacement.verify()?;
            attempted.set(true);
            Ok(())
        },
    );
    let modified = match result {
        Ok(modified) => modified,
        Err(error) => {
            let mut failure = Failure::from(error).context(preserved());
            if attempted.get() {
                failure = failure.context(format!(
                    " · The server file may be partly updated · At least {} replacement bytes acknowledged",
                    acknowledged.get()
                ));
                return Err(ReplaceFailure::uncertain(failure));
            }
            return Err(failure.into());
        }
    };
    let checked = (|| -> Result<()> {
        original.verify()?;
        replacement.verify()?;
        folder.verify(client, false)?;
        backup.verify(client)?;
        require_entry(client, &backup.payload, backup_stamp)?;
        upload
            .progress
            .set_position(progress_at_start + upload.file.size())?;
        Ok(())
    })();
    checked.map_err(|error| {
        ReplaceFailure::uncertain(error.context(format!(
            " · The in-place write completed but final verification failed{}",
            preserved()
        )))
    })?;
    let warning = (!modified).then(|| {
        io::Error::other("The server did not preserve the source modification time").into()
    });
    Ok(Completion {
        backup: backup_path,
        warning,
    })
}

fn save_backup(
    client: &mut Client,
    backup: &RemoteStage,
    original: &SealedFile,
    size: u64,
    upload: &Upload<'_>,
) -> Result<Attributes> {
    backup.verify(client)?;
    let mut owner = None;
    client.upload_private_verified(
        &backup.payload,
        UploadStage::Create,
        size,
        original.file()?,
        |observed, _| {
            if observed.uid != backup.uid {
                return Err(io::Error::other(
                    "The private backup has an unexpected owner",
                ));
            }
            owner = Some(observed);
            original.verify()?;
            (upload.authorize)().map_err(|error| io::Error::other(error.message))
        },
        upload.stopped,
    )?;
    let owner = owner.ok_or_else(|| io::Error::other("The private backup owner is unavailable"))?;
    if client.verify_private_upload(
        &backup.payload,
        owner,
        size,
        original.file()?,
        upload.stopped,
    )? != size
    {
        return Err(io::Error::other("The private backup is incomplete").into());
    }
    original.verify()?;
    backup.verify(client)?;
    let stamp = client.lstat(&backup.payload)?;
    if stamp.kind() != FileKind::File
        || stamp.uid != Some(owner.uid)
        || stamp.gid != Some(owner.gid)
        || stamp.size != Some(size)
        || stamp.permissions.map(|mode| mode & 0o7777) != Some(0o600)
        || client.canonicalize(&backup.payload)? != backup.payload
    {
        return Err(io::Error::other("The private backup changed before replacement").into());
    }
    Ok(stamp)
}
