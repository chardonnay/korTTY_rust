//! Guarded writes to a regular file on the handle used to prove its original bytes.

use std::io::{Seek, SeekFrom};

use super::*;

/// Exact reviewed attributes and resulting size and modification time.
#[derive(Clone, Copy)]
pub struct InPlaceUpload {
    pub original: Attributes,
    pub size: u64,
    pub modified: u32,
}

impl<R: Read, W: Write> Sftp<R, W> {
    /// Updates an existing file without recreating or renaming it.
    ///
    /// The original and replacement must be complete, stable byte streams.
    /// The original is compared before the first mutation. A shorter
    /// replacement is truncated only after the writes have been acknowledged.
    /// `before_write` runs immediately before each potentially mutating
    /// request and receives the acknowledged replacement byte count. A caller
    /// must retain the original and treat a subsequent failure as potentially
    /// partial. The result reports whether the requested modification time
    /// was applied or already present.
    pub fn upload_in_place(
        &mut self,
        path: &[u8],
        upload: InPlaceUpload,
        mut original: impl Read + Seek,
        mut replacement: impl Read + Seek,
        cancelled: impl Fn() -> bool,
        before_write: impl Fn(u64) -> io::Result<()>,
    ) -> Result<bool, Error> {
        let InPlaceUpload {
            original: expected,
            size,
            modified,
        } = upload;
        let old_size = expected.size.ok_or(Error::InvalidInput(
            "In-place upload requires a known original size",
        ))?;
        if !path.starts_with(b"/")
            || expected.kind() != FileKind::File
            || expected.uid.is_none_or(|uid| uid == u32::MAX)
            || expected.gid.is_none_or(|gid| gid == u32::MAX)
            || expected.permissions.is_none_or(|mode| mode & 0o7000 != 0)
            || expected.modified.is_none()
        {
            return Err(Error::InvalidInput("The in-place upload target is invalid"));
        }
        if original.seek(SeekFrom::End(0)).map_err(Error::LocalIo)? != old_size
            || replacement.seek(SeekFrom::End(0)).map_err(Error::LocalIo)? != size
        {
            return Err(Error::SourceChanged);
        }
        check_copy_cancelled(&cancelled)?;
        check_regular(expected, self.lstat(path)?)?;
        if self.canonicalize(path)? != path {
            return Err(Error::SourceChanged);
        }
        let mut fields = path_fields(path, false)?;
        fields.extend_from_slice(&(OPEN_READ | OPEN_WRITE).to_be_bytes());
        fields.extend_from_slice(&0u32.to_be_bytes());
        let handle = self.open_handle(wire::OPEN, &fields)?;
        let result = self.with_handle(&handle, |client, handle| {
            client.check_in_place(path, handle, expected)?;
            original.rewind().map_err(Error::LocalIo)?;
            client.check_complete_prefix(handle, old_size, &mut original, &cancelled)?;
            client.check_in_place(path, handle, expected)?;
            replacement.rewind().map_err(Error::LocalIo)?;

            let mut fields = handle_fields(handle)?;
            let handle_len = fields.len();
            let mut offset = 0;
            let mut chunk = [0; CHUNK];
            while offset < size {
                check_copy_cancelled(&cancelled)?;
                let count = (size - offset).min(CHUNK as u64) as usize;
                replacement
                    .read_exact(&mut chunk[..count])
                    .map_err(Error::LocalIo)?;
                fields.truncate(handle_len);
                fields.extend_from_slice(&offset.to_be_bytes());
                wire::string(&mut fields, &chunk[..count])?;
                before_write(offset).map_err(Error::LocalIo)?;
                client.status_request(wire::WRITE, &fields)?;
                offset += count as u64;
            }

            check_copy_cancelled(&cancelled)?;
            let written = client.handle_attributes(handle)?;
            check_written_attributes(expected, written, old_size.max(size))?;
            client.check_in_place(path, handle, written)?;
            if size < old_size {
                fields.truncate(handle_len);
                fields.extend_from_slice(&wire::ATTR_SIZE.to_be_bytes());
                fields.extend_from_slice(&size.to_be_bytes());
                before_write(size).map_err(Error::LocalIo)?;
                client.status_request(wire::FSETSTAT, &fields)?;
            }

            let written = client.handle_attributes(handle)?;
            check_written_attributes(expected, written, size)?;
            client.check_in_place(path, handle, written)?;
            replacement.rewind().map_err(Error::LocalIo)?;
            client.check_complete_prefix(handle, size, &mut replacement, &cancelled)?;
            client.check_in_place(path, handle, written)?;
            let mut final_attrs = written;
            if written.modified != Some(modified)
                && let Some(accessed) = written.accessed
            {
                fields.truncate(handle_len);
                fields.extend_from_slice(&wire::ATTR_TIMES.to_be_bytes());
                fields.extend_from_slice(&accessed.to_be_bytes());
                fields.extend_from_slice(&modified.to_be_bytes());
                check_copy_cancelled(&cancelled)?;
                before_write(size).map_err(Error::LocalIo)?;
                match client.status_request(wire::FSETSTAT, &fields) {
                    Ok(()) => final_attrs.modified = Some(modified),
                    Err(Error::Status(status))
                        if matches!(
                            status.code,
                            StatusCode::PermissionDenied
                                | StatusCode::Unsupported
                                | StatusCode::Failure
                        ) => {}
                    Err(error) => return Err(error),
                }
                client.check_in_place(path, handle, final_attrs)?;
            }
            Ok(final_attrs)
        });
        if self.failed && result.as_ref().is_err_and(|error| !error.connection_lost()) {
            return Err(Error::Transport(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "The SFTP connection ended during the in-place upload",
            )));
        }
        let final_attrs = result?;
        check_regular(final_attrs, self.lstat(path)?)?;
        if self.canonicalize(path)? != path {
            return Err(Error::SourceChanged);
        }
        Ok(final_attrs.modified == Some(modified))
    }

    fn check_in_place(
        &mut self,
        path: &[u8],
        handle: &[u8],
        expected: Attributes,
    ) -> Result<(), Error> {
        check_regular(expected, self.handle_attributes(handle)?)?;
        check_regular(expected, self.lstat(path)?)?;
        if self.canonicalize(path)? != path {
            return Err(Error::SourceChanged);
        }
        Ok(())
    }
}

fn check_written_attributes(
    original: Attributes,
    current: Attributes,
    size: u64,
) -> Result<(), Error> {
    if current.uid != original.uid
        || current.gid != original.gid
        || current.permissions != original.permissions
        || current.modified.is_none()
        || current.size != Some(size)
    {
        return Err(Error::SourceChanged);
    }
    Ok(())
}
