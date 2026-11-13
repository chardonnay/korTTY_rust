//! Exclusive SFTP upload stages and complete prefix checks on the resumed write handle.

use std::io::{Seek, SeekFrom};

use super::*;

/// The owner and group observed on an exclusively created private upload stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadOwner {
    pub uid: u32,
    pub gid: u32,
}

/// Reviewed final ownership and mode for a completed private upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadMetadata {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub mode: Option<u32>,
}

/// Opening mode for a new or previously recorded private upload stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadStage {
    Create,
    Resume(UploadOwner),
}

struct UploadOptions {
    stage: UploadStage,
    modified: Option<u32>,
}

impl<R: Read, W: Write> Sftp<R, W> {
    /// Writes an exclusive private stage, or resumes it at its actual verified size.
    /// `checkpoint` runs before the first write on the same handle. A caller can use it
    /// to persist ownership before a transport interruption. The final attributes
    /// must still match the original private stage and the expected complete length.
    pub fn upload_private_verified(
        &mut self,
        path: &[u8],
        stage: UploadStage,
        expected: u64,
        source: impl Read + Seek,
        checkpoint: impl FnMut(UploadOwner, u64) -> io::Result<()>,
        cancelled: impl Fn() -> bool,
    ) -> Result<u64, Error> {
        self.upload_private_stage(
            path,
            UploadOptions {
                stage,
                modified: None,
            },
            expected,
            source,
            checkpoint,
            cancelled,
        )
    }

    /// Writes a new exclusive stage and sets its reviewed modification time on the open handle.
    /// `modified` is a whole Unix second representable by SFTP v3. Failure leaves the stage for
    /// the caller to clean up and must prevent publication.
    pub fn upload_private_with_modified(
        &mut self,
        path: &[u8],
        expected: u64,
        modified: u32,
        source: impl Read + Seek,
        checkpoint: impl FnMut(UploadOwner, u64) -> io::Result<()>,
        cancelled: impl Fn() -> bool,
    ) -> Result<u64, Error> {
        self.upload_private_stage(
            path,
            UploadOptions {
                stage: UploadStage::Create,
                modified: Some(modified),
            },
            expected,
            source,
            checkpoint,
            cancelled,
        )
    }

    fn upload_private_stage(
        &mut self,
        path: &[u8],
        options: UploadOptions,
        expected: u64,
        mut source: impl Read + Seek,
        mut checkpoint: impl FnMut(UploadOwner, u64) -> io::Result<()>,
        cancelled: impl Fn() -> bool,
    ) -> Result<u64, Error> {
        let UploadOptions { stage, modified } = options;
        check_copy_cancelled(&cancelled)?;
        let (owner, before, flags) = match stage {
            UploadStage::Create => (
                None,
                None,
                OPEN_READ | OPEN_WRITE | OPEN_CREATE | OPEN_EXCLUSIVE,
            ),
            UploadStage::Resume(owner) => {
                let before = self.lstat(path)?;
                private_attributes(before, Some(owner), expected)?;
                (Some(owner), Some(before), OPEN_READ | OPEN_WRITE)
            }
        };
        let mut fields = path_fields(path, false)?;
        fields.extend_from_slice(&flags.to_be_bytes());
        if stage == UploadStage::Create {
            push_permissions(&mut fields, 0o600);
        } else {
            fields.extend_from_slice(&0u32.to_be_bytes());
        }
        let handle = self.open_handle(wire::OPEN, &fields)?;
        self.with_handle(&handle, |client, handle| {
            let attrs = client.handle_attributes(handle)?;
            let (owner, offset) = private_attributes(attrs, owner, expected)?;
            if stage == UploadStage::Create && offset != 0
                || before.is_some_and(|previous| !same_private(previous, attrs))
                || !same_private(client.lstat(path)?, attrs)
            {
                return Err(Error::SourceChanged);
            }
            client.check_upload_prefix(handle, offset, &mut source, &cancelled)?;
            if !same_private(client.handle_attributes(handle)?, attrs)
                || !same_private(client.lstat(path)?, attrs)
            {
                return Err(Error::SourceChanged);
            }
            check_copy_cancelled(&cancelled)?;
            checkpoint(owner, offset).map_err(Error::LocalIo)?;
            client.write_upload_suffix(handle, &mut source, offset, expected, &cancelled)?;
            let after = client.handle_attributes(handle)?;
            let (_, length) = private_attributes(after, Some(owner), expected)?;
            if length != expected || !same_private(client.lstat(path)?, after) {
                return Err(Error::SourceChanged);
            }
            check_copy_cancelled(&cancelled)?;
            if let Some(modified) = modified {
                client.set_upload_modified(path, handle, after, modified, || Ok(()))?;
                check_copy_cancelled(&cancelled)?;
            }
            Ok(expected)
        })
    }

    fn set_upload_modified(
        &mut self,
        path: &[u8],
        handle: &[u8],
        before: Attributes,
        modified: u32,
        before_change: impl FnOnce() -> io::Result<()>,
    ) -> Result<Attributes, Error> {
        let accessed = before.accessed.ok_or(Error::Unsupported(
            "The server did not report the staged file access time",
        ))?;
        let mut fields = handle_fields(handle)?;
        fields.extend_from_slice(&wire::ATTR_TIMES.to_be_bytes());
        fields.extend_from_slice(&accessed.to_be_bytes());
        fields.extend_from_slice(&modified.to_be_bytes());
        let expected = Attributes {
            modified: Some(modified),
            ..before
        };
        before_change().map_err(Error::LocalIo)?;
        self.set_upload_attributes(path, handle, &fields, expected)?;
        Ok(expected)
    }

    /// Compares a saved private stage or published destination without changing it.
    /// Returns the remote size. A caller can require a complete length before accepting publication.
    pub fn verify_private_upload(
        &mut self,
        path: &[u8],
        owner: UploadOwner,
        maximum: u64,
        source: impl Read + Seek,
        cancelled: impl Fn() -> bool,
    ) -> Result<u64, Error> {
        check_copy_cancelled(&cancelled)?;
        let before = self.lstat(path)?;
        private_attributes(before, Some(owner), maximum)?;
        self.with_verified_upload(path, before, source, &cancelled, |_, _, size| Ok(size))
    }

    /// Verifies all bytes of a completed 0600 stage, then sets metadata on the
    /// same handle, including all twelve POSIX mode bits when supplied. The
    /// supplied attributes are updated after each verified write.
    /// `before_change` rechecks the caller's permission just before each write.
    /// An error prevents publication. A caller can use the last verified attributes
    /// to clean up a private stage after a partial change.
    pub fn set_private_upload_metadata(
        &mut self,
        path: &[u8],
        verified: &mut Attributes,
        source: impl Read + Seek,
        metadata: UploadMetadata,
        cancelled: impl Fn() -> bool,
        before_change: impl Fn() -> io::Result<()>,
    ) -> Result<(), Error> {
        self.finalize_private_upload(
            path,
            verified,
            source,
            (metadata, None),
            cancelled,
            before_change,
        )
    }

    /// Checks all bytes and applies the reviewed metadata and modification time.
    /// The callback is reached only at an actual attribute write.
    pub fn finalize_private_upload(
        &mut self,
        path: &[u8],
        verified: &mut Attributes,
        mut source: impl Read + Seek,
        (metadata, modified): (UploadMetadata, Option<u32>),
        cancelled: impl Fn() -> bool,
        mut before_change: impl FnMut() -> io::Result<()>,
    ) -> Result<(), Error> {
        if metadata.mode.is_some_and(|mode| mode > 0o7777)
            || metadata.uid == Some(u32::MAX)
            || metadata.gid == Some(u32::MAX)
        {
            return Err(Error::InvalidInput("Invalid private upload metadata"));
        }
        check_copy_cancelled(&cancelled)?;
        let (original, size) = private_attributes(*verified, None, u64::MAX)?;
        if original.uid == u32::MAX
            || original.gid == u32::MAX
            || source.seek(SeekFrom::End(0)).map_err(Error::LocalIo)? != size
        {
            return Err(Error::SourceChanged);
        }
        let owner = UploadOwner {
            uid: metadata.uid.unwrap_or(original.uid),
            gid: metadata.gid.unwrap_or(original.gid),
        };
        self.with_verified_upload(path, *verified, source, &cancelled, |client, handle, _| {
            if owner != original {
                let mut fields = handle_fields(handle)?;
                fields.extend_from_slice(&wire::ATTR_UID_GID.to_be_bytes());
                fields.extend_from_slice(&owner.uid.to_be_bytes());
                fields.extend_from_slice(&owner.gid.to_be_bytes());
                let expected = Attributes {
                    uid: Some(owner.uid),
                    gid: Some(owner.gid),
                    ..*verified
                };
                before_change().map_err(Error::LocalIo)?;
                client.set_upload_attributes(path, handle, &fields, expected)?;
                *verified = expected;
            }
            check_copy_cancelled(&cancelled)?;
            if let Some(modified) = modified.filter(|time| verified.modified != Some(*time)) {
                *verified = client.set_upload_modified(
                    path,
                    handle,
                    *verified,
                    modified,
                    &mut before_change,
                )?;
            }
            check_copy_cancelled(&cancelled)?;
            if let Some(mode) = metadata.mode
                && let Some(previous) = verified.permissions
                && mode != previous & 0o7777
            {
                let mut fields = handle_fields(handle)?;
                push_permissions(&mut fields, mode);
                let expected = Attributes {
                    permissions: Some((previous & !0o7777) | mode),
                    ..*verified
                };
                before_change().map_err(Error::LocalIo)?;
                client.set_upload_attributes(path, handle, &fields, expected)?;
                *verified = expected;
            }
            check_copy_cancelled(&cancelled)
        })
    }

    fn set_upload_attributes(
        &mut self,
        path: &[u8],
        handle: &[u8],
        fields: &[u8],
        expected: Attributes,
    ) -> Result<(), Error> {
        self.status_request(wire::FSETSTAT, fields)?;
        if !same_private(expected, self.handle_attributes(handle)?)
            || !same_private(expected, self.lstat(path)?)
        {
            return Err(Error::SourceChanged);
        }
        Ok(())
    }

    fn with_verified_upload<T>(
        &mut self,
        path: &[u8],
        before: Attributes,
        mut source: impl Read + Seek,
        cancelled: &impl Fn() -> bool,
        finish: impl FnOnce(&mut Self, &[u8], u64) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let size = before.size.ok_or(Error::SourceChanged)?;
        check_copy_cancelled(cancelled)?;
        let handle = self.open_read_file(path)?;
        self.with_handle(&handle, |client, handle| {
            if !same_private(client.handle_attributes(handle)?, before)
                || !same_private(client.lstat(path)?, before)
            {
                return Err(Error::SourceChanged);
            }
            client.check_upload_prefix(handle, size, &mut source, cancelled)?;
            if !same_private(client.handle_attributes(handle)?, before)
                || !same_private(client.lstat(path)?, before)
            {
                return Err(Error::SourceChanged);
            }
            check_copy_cancelled(cancelled)?;
            finish(client, handle, size)
        })
    }

    fn check_upload_prefix(
        &mut self,
        handle: &[u8],
        length: u64,
        source: &mut (impl Read + Seek),
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), Error> {
        source.seek(SeekFrom::Start(0)).map_err(Error::LocalIo)?;
        self.check_complete_prefix(handle, length, source, cancelled)
    }

    fn write_upload_suffix(
        &mut self,
        handle: &[u8],
        source: &mut impl Read,
        mut offset: u64,
        total: u64,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), Error> {
        let mut fields = handle_fields(handle)?;
        let handle_end = fields.len();
        let mut buffer = [0; CHUNK];
        while offset < total {
            check_copy_cancelled(cancelled)?;
            let maximum = (total - offset).min(CHUNK as u64) as usize;
            let count = match source.read(&mut buffer[..maximum]) {
                Ok(0) => return Err(Error::SourceChanged),
                Ok(count) if count <= maximum => count,
                Ok(_) => return Err(Error::LocalIo(io::ErrorKind::InvalidData.into())),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::LocalIo(error)),
            };
            fields.truncate(handle_end);
            fields.extend_from_slice(&offset.to_be_bytes());
            wire::string(&mut fields, &buffer[..count])?;
            self.status_request(wire::WRITE, &fields)?;
            offset += count as u64;
        }
        check_copy_cancelled(cancelled)
    }
}

fn private_attributes(
    attrs: Attributes,
    owner: Option<UploadOwner>,
    maximum: u64,
) -> Result<(UploadOwner, u64), Error> {
    let (Some(uid), Some(gid), Some(size)) = (attrs.uid, attrs.gid, attrs.size) else {
        return Err(Error::SourceChanged);
    };
    let observed = UploadOwner { uid, gid };
    if attrs.kind() != FileKind::File
        || attrs.permissions.map(|bits| bits & 0o7777) != Some(0o600)
        || attrs.modified.is_none()
        || size > maximum
        || owner.is_some_and(|owner| observed != owner)
    {
        return Err(Error::SourceChanged);
    }
    Ok((observed, size))
}

fn same_private(left: Attributes, right: Attributes) -> bool {
    left.without_access_time() == right.without_access_time()
}
