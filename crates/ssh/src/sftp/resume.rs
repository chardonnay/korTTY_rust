//! Complete prefix verification on the handle used for the resumed download.

use super::*;

impl<R: Read, W: Write> Sftp<R, W> {
    /// Resumes only after comparing every saved byte with the open remote file.
    /// The caller supplies a prefix reader at offset zero and a destination at
    /// the resume offset. A shared file cursor reaches that offset during the
    /// comparison. Reader errors also permit caller-controlled cancellation.
    pub fn download_regular_with_prefix(
        &mut self,
        path: &[u8],
        expected: Attributes,
        offset: u64,
        mut prefix: impl Read,
        destination: impl Write,
    ) -> Result<u64, Error> {
        if expected.modified.is_none() || expected.size.is_none_or(|size| offset > size) {
            return Err(Error::InvalidInput("Invalid SFTP download resume point"));
        }
        self.download_verified(path, expected, offset, destination, |client, handle| {
            client.check_complete_prefix(handle, offset, &mut prefix, &|| false)
        })
    }

    pub(super) fn download_verified(
        &mut self,
        path: &[u8],
        expected: Attributes,
        offset: u64,
        mut destination: impl Write,
        verify: impl FnOnce(&mut Self, &[u8]) -> Result<(), Error>,
    ) -> Result<u64, Error> {
        check_regular(expected, self.lstat(path)?)?;
        let handle = self.open_read_file(path)?;
        let count = self.with_handle(&handle, |client, handle| {
            let opened = client.handle_attributes(handle)?;
            check_regular_handle(path, expected, opened)?;
            check_regular(expected, client.lstat(path)?)?;
            verify(client, handle)?;
            let count = client.copy_download(handle, offset, expected.size, &mut destination)?;
            if expected.size.is_some_and(|size| size != count) {
                return Err(Error::SourceChanged);
            }
            check_regular(opened, client.handle_attributes(handle)?)?;
            destination.flush().map_err(Error::LocalIo)?;
            Ok(count)
        })?;
        check_regular(expected, self.lstat(path)?)?;
        Ok(count)
    }

    pub(super) fn check_complete_prefix(
        &mut self,
        handle: &[u8],
        length: u64,
        prefix: &mut impl Read,
        cancelled: &impl Fn() -> bool,
    ) -> Result<(), Error> {
        let mut fields = handle_fields(handle)?;
        let handle_end = fields.len();
        let mut offset = 0;
        let mut local = [0; CHUNK];
        while offset < length {
            check_copy_cancelled(cancelled)?;
            fields.truncate(handle_end);
            let count = (length - offset).min(CHUNK as u64) as usize;
            let read = self.read_download_chunk(&mut fields, offset, count, |remote| {
                read_prefix(prefix, &mut local[..remote.len()])?;
                if remote != &local[..remote.len()] {
                    return Err(Error::SourceChanged);
                }
                Ok(())
            });
            if is_eof(&read) {
                return Err(Error::SourceChanged);
            }
            offset += read? as u64;
        }
        Ok(())
    }
}

fn read_prefix(source: &mut impl Read, mut output: &mut [u8]) -> Result<(), Error> {
    while !output.is_empty() {
        // Cancellation readers intentionally use Interrupted. Preserve that
        // error instead of retrying it as Read::read_exact would do.
        let count = source.read(output).map_err(Error::LocalIo)?;
        if count == 0 || count > output.len() {
            return Err(Error::LocalIo(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Invalid SFTP prefix read length",
            )));
        }
        output = &mut output[count..];
    }
    Ok(())
}
