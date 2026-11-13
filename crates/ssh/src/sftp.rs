//! Blocking SFTP v3 client for a byte stream, usually an authenticated SSH subsystem.
//!
//! Run requests on a worker thread. The client sends one request at a time and
//! invalidates the stream after a transport or protocol error.

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;

mod edit;
mod in_place;
mod resume;
mod upload_resume;
mod wire;

pub use edit::{MAX_EDIT_BYTES, RemoteFile, RemoteText};
pub use in_place::InPlaceUpload;
pub use upload_resume::{UploadMetadata, UploadOwner, UploadStage};
use wire::Decoder;

const VERSION: u32 = 3;
const POSIX_RENAME: &[u8] = b"posix-rename@openssh.com";
const LSETSTAT: &[u8] = b"lsetstat@openssh.com";
const COPY_DATA: &[u8] = b"copy-data";
const HARDLINK: &[u8] = b"hardlink@openssh.com";
const COPY_CHUNK: u64 = 8 * 1024 * 1024;
const MAX_PATH: usize = 16 * 1024;
const MAX_HANDLE: usize = 256;
const CHUNK: usize = 32 * 1024;
/// Bytes before a resume point which must match the checked remote handle.
pub const DOWNLOAD_OVERLAP_BYTES: usize = 64 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_DIRECTORY_BYTES: usize = 32 * 1024 * 1024;

const OPEN_READ: u32 = 0x0000_0001;
const OPEN_WRITE: u32 = 0x0000_0002;
const OPEN_CREATE: u32 = 0x0000_0008;
const OPEN_EXCLUSIVE: u32 = 0x0000_0020;

/// An SFTP v3 or vendor-defined status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCode {
    Ok,
    Eof,
    NoSuchFile,
    PermissionDenied,
    Failure,
    BadMessage,
    NoConnection,
    ConnectionLost,
    Unsupported,
    Other(u32),
}

impl From<u32> for StatusCode {
    fn from(code: u32) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::Eof,
            2 => Self::NoSuchFile,
            3 => Self::PermissionDenied,
            4 => Self::Failure,
            5 => Self::BadMessage,
            6 => Self::NoConnection,
            7 => Self::ConnectionLost,
            8 => Self::Unsupported,
            code => Self::Other(code),
        }
    }
}

/// The server's status and bounded, display-safe message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatus {
    pub code: StatusCode,
    pub message: String,
}

/// An SFTP operation failure. A failed transport cannot be reused.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Transport(io::Error),
    LocalIo(io::Error),
    Version(u32),
    Unsupported(&'static str),
    InvalidInput(&'static str),
    Protocol(&'static str),
    Limit(&'static str),
    SourceChanged,
    EditConflict(&'static str),
    SaveUncertain(Box<Error>),
    Status(RemoteStatus),
    Closed,
}

impl Error {
    /// True if the server reported the v3 status `SSH_FX_NO_SUCH_FILE`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Status(status) if status.code == StatusCode::NoSuchFile)
    }

    /// True if this protocol stream is unusable and must be reopened.
    pub fn connection_lost(&self) -> bool {
        match self {
            Self::Transport(_)
            | Self::Version(_)
            | Self::Protocol(_)
            | Self::Closed
            | Self::SaveUncertain(_) => true,
            Self::Status(status) => matches!(
                status.code,
                StatusCode::NoConnection | StatusCode::ConnectionLost | StatusCode::BadMessage
            ),
            _ => false,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Transport(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "SFTP transport error ({error})"),
            Self::LocalIo(error) => write!(formatter, "Local SFTP transfer error ({error})"),
            Self::Version(version) => write!(formatter, "Unsupported SFTP version {version}"),
            Self::InvalidInput(message) | Self::Protocol(message) | Self::Unsupported(message) | Self::EditConflict(message) => {
                formatter.write_str(message)
            }
            Self::Limit(message) => write!(formatter, "SFTP limit exceeded ({message})"),
            Self::SourceChanged => formatter.write_str("The remote file changed during transfer"),
            Self::SaveUncertain(_) => formatter.write_str("Could not confirm remote save. Reopen the file and verify its contents before retrying."),
            Self::Status(status) => {
                if status.message.is_empty() {
                    write!(formatter, "SFTP server returned {:?}", status.code)
                } else {
                    write!(
                        formatter,
                        "SFTP server returned {:?} ({})",
                        status.code, status.message
                    )
                }
            }
            Self::Closed => formatter.write_str("SFTP channel is closed"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) | Self::LocalIo(error) => Some(error),
            Self::SaveUncertain(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// Type reported in the POSIX mode bits of a v3 attribute record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Special,
    Unknown,
}

/// Metadata supplied by the SFTP server. Timestamps are UTC Unix seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attributes {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub permissions: Option<u32>,
    pub accessed: Option<u32>,
    pub modified: Option<u32>,
}

impl Attributes {
    /// Derives the file type from the optional POSIX mode.
    pub fn kind(self) -> FileKind {
        match self.permissions.unwrap_or(0) & 0o170000 {
            0o100000 => FileKind::File,
            0o040000 => FileKind::Directory,
            0o120000 => FileKind::Symlink,
            0 => FileKind::Unknown,
            _ => FileKind::Special,
        }
    }

    fn without_access_time(self) -> Self {
        Self {
            accessed: None,
            ..self
        }
    }
}

/// A single remote directory entry, with its name preserved as wire bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: Vec<u8>,
    pub attrs: Attributes,
}

/// Owns the two ends of a single, negotiated SFTP v3 session.
pub struct Sftp<R, W> {
    reader: R,
    writer: W,
    next_id: u32,
    identity: Arc<()>,
    posix_rename: bool,
    lsetstat: bool,
    copy_data: bool,
    hardlink: bool,
    failed: bool,
}

impl<R: Read, W: Write> Sftp<R, W> {
    /// Sends `INIT` and validates the server's v3 `VERSION` response.
    pub fn new(mut reader: R, mut writer: W) -> Result<Self, Error> {
        let mut init = vec![wire::INIT];
        init.extend_from_slice(&VERSION.to_be_bytes());
        wire::send(&mut writer, &init)?;
        let response = wire::receive(&mut reader)?;
        let mut decoder = Decoder::new(&response);
        if decoder.byte()? != wire::VERSION {
            return Err(Error::Protocol("expected SFTP VERSION response"));
        }
        let version = decoder.u32()?;
        if version != VERSION {
            return Err(Error::Version(version));
        }
        let mut posix_rename = false;
        let mut lsetstat = false;
        let mut copy_data = false;
        let mut hardlink = false;
        while decoder.length() != 0 {
            let name = decoder.string()?;
            let data = decoder.string()?;
            posix_rename |= name == POSIX_RENAME && data == b"1";
            lsetstat |= name == LSETSTAT && data == b"1";
            copy_data |= name == COPY_DATA && data == b"1";
            hardlink |= name == HARDLINK && data == b"1";
        }
        Ok(Self {
            reader,
            writer,
            next_id: 1,
            identity: Arc::new(()),
            posix_rename,
            lsetstat,
            copy_data,
            hardlink,
            failed: false,
        })
    }

    /// Resolves a server path, with `.` or an empty path denoting the login directory.
    pub fn canonicalize(&mut self, path: impl AsRef<[u8]>) -> Result<Vec<u8>, Error> {
        let fields = path_fields(path.as_ref(), true)?;
        self.request(wire::REALPATH, &fields, wire::NAME, |decoder| {
            if decoder.u32()? != 1 {
                return Err(Error::Protocol(
                    "SFTP REALPATH did not return a single name",
                ));
            }
            let entry = decode_entry(decoder)?;
            if !entry.name.starts_with(b"/") || entry.name.contains(&0) {
                return Err(Error::Protocol("invalid SFTP canonical path"));
            }
            Ok(entry.name)
        })
    }

    /// Lists at most 100,000 entries and 32 MiB of response payloads, skipping `.` and `..`.
    pub fn read_dir(&mut self, path: impl AsRef<[u8]>) -> Result<Vec<DirectoryEntry>, Error> {
        self.read_dir_limited(path, MAX_DIRECTORY_ENTRIES, MAX_DIRECTORY_BYTES)
    }

    /// Lists a directory subject to the caller's tighter entry and payload budgets.
    pub fn read_dir_limited(
        &mut self,
        path: impl AsRef<[u8]>,
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<Vec<DirectoryEntry>, Error> {
        let fields = path_fields(path.as_ref(), true)?;
        let handle = self.open_handle(wire::OPENDIR, &fields)?;
        self.with_handle(&handle, |client, handle| {
            client.read_directory(
                handle,
                max_entries.min(MAX_DIRECTORY_ENTRIES),
                max_bytes.min(MAX_DIRECTORY_BYTES),
            )
        })
    }

    /// Retrieves metadata, following the final symbolic link.
    pub fn stat(&mut self, path: impl AsRef<[u8]>) -> Result<Attributes, Error> {
        self.path_attributes(wire::STAT, path.as_ref())
    }

    /// Retrieves metadata for the final directory entry, including symbolic links.
    pub fn lstat(&mut self, path: impl AsRef<[u8]>) -> Result<Attributes, Error> {
        self.path_attributes(wire::LSTAT, path.as_ref())
    }

    /// Copies one remote file into a caller-owned writer and flushes it.
    ///
    /// On failure the writer may contain partial data. To protect an existing
    /// local file, pass a staging file and publish it only after success.
    pub fn download(
        &mut self,
        path: impl AsRef<[u8]>,
        mut destination: impl Write,
    ) -> Result<u64, Error> {
        let mut fields = path_fields(path.as_ref(), false)?;
        fields.extend_from_slice(&OPEN_READ.to_be_bytes());
        fields.extend_from_slice(&0u32.to_be_bytes());
        let handle = self.open_handle(wire::OPEN, &fields)?;
        self.with_handle(&handle, |client, handle| {
            let offset = client.copy_download(handle, 0, None, &mut destination)?;
            destination.flush().map_err(Error::LocalIo)?;
            Ok(offset)
        })
    }

    /// Streams a checked regular file. Matches the open handle to the expected
    /// v3 attributes and rechecks the path before and after the read.
    ///
    /// SFTP v3 offers no atomic no-follow open or stable file identifier.
    /// A caller should stage the output and only publish it on success.
    pub fn download_regular(
        &mut self,
        path: &[u8],
        expected: Attributes,
        destination: impl Write,
    ) -> Result<u64, Error> {
        self.download_regular_from(path, expected, 0, &[], destination)
    }

    /// Continues a checked regular file at a caller-owned staging offset.
    ///
    /// The overlap is the last 64 KiB of the saved prefix, or the whole
    /// prefix when it is shorter. Its bytes must match on the same remote
    /// handle used for copying. The writer must already be positioned at the
    /// offset. Returns the final size including the saved prefix.
    pub fn download_regular_from(
        &mut self,
        path: &[u8],
        expected: Attributes,
        offset: u64,
        overlap: &[u8],
        destination: impl Write,
    ) -> Result<u64, Error> {
        if overlap.len() as u64 != offset.min(DOWNLOAD_OVERLAP_BYTES as u64)
            || (offset != 0
                && (expected.modified.is_none() || expected.size.is_none_or(|size| offset > size)))
        {
            return Err(Error::InvalidInput("Invalid SFTP download resume point"));
        }
        self.download_verified(path, expected, offset, destination, |client, handle| {
            if !overlap.is_empty() {
                client.check_download_overlap(handle, offset, overlap)?;
            }
            Ok(())
        })
    }

    /// Streams into a new remote file with requested mode `0600` and exclusive creation.
    ///
    /// On failure the newly created file may remain partially written. Supply
    /// a unique same-directory staging path and publish with [`Self::rename`]
    /// or [`Self::replace`] after successful completion if required.
    pub fn upload_new(
        &mut self,
        path: impl AsRef<[u8]>,
        mut source: impl Read,
    ) -> Result<u64, Error> {
        let handle = self.open_new_private_file(path.as_ref())?;
        self.with_handle(&handle, |client, handle| {
            client.copy_upload(handle, &mut source)
        })
    }

    /// Uploads to an exclusively created file and sets its final mode through the open handle.
    /// The returned attributes describe the handle after setting its mode. Use a private staging
    /// path because a failed upload can leave a partial file.
    pub fn upload_new_with_permissions(
        &mut self,
        path: impl AsRef<[u8]>,
        mut source: impl Read,
        mode: u32,
    ) -> Result<Attributes, Error> {
        let handle = self.open_new_private_file(path.as_ref())?;
        self.with_handle(&handle, |client, handle| {
            let count = client.copy_upload(handle, &mut source)?;
            let mut fields = handle_fields(handle)?;
            push_permissions(&mut fields, mode);
            client.status_request(wire::FSETSTAT, &fields)?;
            let attrs = client.handle_attributes(handle)?;
            if attrs.kind() != FileKind::File
                || attrs.size != Some(count)
                || attrs.permissions.map(|bits| bits & 0o7777) != Some(mode & 0o7777)
            {
                return Err(Error::SourceChanged);
            }
            Ok(attrs)
        })
    }

    /// Returns whether the server advertised version 1 of `copy-data`.
    pub fn supports_copy_data(&self) -> bool {
        self.copy_data
    }

    /// Copies exactly the checked regular source's reported size inside the server.
    /// The destination is opened exclusively with mode `0600`. A failure may
    /// leave a partial destination, so callers should pass a private stage path.
    pub fn copy_regular_new(
        &mut self,
        source: &[u8],
        expected: Attributes,
        destination: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> Result<u64, Error> {
        self.copy_regular_new_with_progress(source, expected, destination, cancelled, |_| {})
    }

    /// Copies a checked regular file and reports each completed server-side copy offset.
    pub fn copy_regular_new_with_progress(
        &mut self,
        source: &[u8],
        expected: Attributes,
        destination: &[u8],
        cancelled: impl Fn() -> bool,
        mut progress: impl FnMut(u64),
    ) -> Result<u64, Error> {
        if !self.supports_copy_data() {
            return Err(Error::Unsupported("Server-side copy was not advertised"));
        }
        if source == destination {
            return Err(Error::InvalidInput(
                "The SFTP copy requires different paths",
            ));
        }
        let size = expected.size.ok_or(Error::InvalidInput(
            "The SFTP copy requires a known source size",
        ))?;
        check_copy_cancelled(&cancelled)?;
        check_regular(expected, self.lstat(source)?)?;
        let read = self.open_read_file(source)?;
        self.with_handle(&read, |client, read| {
            check_regular(expected, client.handle_attributes(read)?)?;
            check_regular(expected, client.lstat(source)?)?;
            check_copy_cancelled(&cancelled)?;
            let write = client.open_new_private_file(destination)?;
            if write == read {
                client.failed = true;
                return Err(Error::Protocol("SFTP server reused a live file handle"));
            }
            client.with_handle(&write, |client, write| {
                let mut offset = 0;
                while offset < size {
                    check_copy_cancelled(&cancelled)?;
                    let count = (size - offset).min(COPY_CHUNK);
                    let mut fields = Vec::new();
                    wire::string(&mut fields, COPY_DATA)?;
                    wire::string(&mut fields, read)?;
                    fields.extend_from_slice(&offset.to_be_bytes());
                    fields.extend_from_slice(&count.to_be_bytes());
                    wire::string(&mut fields, write)?;
                    fields.extend_from_slice(&offset.to_be_bytes());
                    client.status_request(wire::EXTENDED, &fields)?;
                    offset += count;
                    progress(offset);
                }
                let copied = client.handle_attributes(write)?;
                if copied.kind() != FileKind::File || copied.size != Some(size) {
                    return Err(Error::SourceChanged);
                }
                Ok(())
            })?;
            check_regular(expected, client.handle_attributes(read)?)
        })?;
        check_regular(expected, self.lstat(source)?)?;
        Ok(size)
    }

    /// Creates an empty directory using the server's default mode and umask.
    pub fn mkdir(&mut self, path: impl AsRef<[u8]>) -> Result<(), Error> {
        let mut fields = path_fields(path.as_ref(), false)?;
        fields.extend_from_slice(&0u32.to_be_bytes());
        self.status_request(wire::MKDIR, &fields)
    }

    /// Exclusively creates a directory with requested POSIX mode `0700`.
    pub fn mkdir_private(&mut self, path: impl AsRef<[u8]>) -> Result<(), Error> {
        self.mkdir_with_permissions(path, 0o700)
    }

    /// Exclusively creates a directory with the requested mode, subject to the server's umask.
    pub fn mkdir_with_permissions(
        &mut self,
        path: impl AsRef<[u8]>,
        mode: u32,
    ) -> Result<(), Error> {
        let mut fields = path_fields(path.as_ref(), false)?;
        push_permissions(&mut fields, mode);
        self.status_request(wire::MKDIR, &fields)
    }

    /// Renames an entry using v3 semantics, which reject an existing destination.
    pub fn rename(&mut self, from: impl AsRef<[u8]>, to: impl AsRef<[u8]>) -> Result<(), Error> {
        let fields = rename_fields(from.as_ref(), to.as_ref(), None)?;
        self.status_request(wire::RENAME, &fields)
    }

    /// Returns whether the server advertised version 1 of `posix-rename@openssh.com`.
    pub fn supports_atomic_replace(&self) -> bool {
        self.posix_rename
    }

    /// Returns whether the server advertised version 1 of `hardlink@openssh.com`.
    pub fn supports_hardlink(&self) -> bool {
        self.hardlink
    }

    /// Links a staged regular file to a new path, failing if that path already exists.
    pub fn hardlink(&mut self, from: impl AsRef<[u8]>, to: impl AsRef<[u8]>) -> Result<(), Error> {
        if !self.supports_hardlink() {
            return Err(Error::Unsupported(
                "Server-side hard link was not advertised",
            ));
        }
        let fields = rename_fields(from.as_ref(), to.as_ref(), Some(HARDLINK))?;
        self.status_request(wire::EXTENDED, &fields)
    }

    /// Renames an entry with POSIX replacement semantics when the server advertises support.
    pub fn replace(&mut self, from: impl AsRef<[u8]>, to: impl AsRef<[u8]>) -> Result<(), Error> {
        if !self.supports_atomic_replace() {
            return Err(Error::Unsupported("Atomic replacement was not advertised"));
        }
        let fields = rename_fields(from.as_ref(), to.as_ref(), Some(POSIX_RENAME))?;
        self.status_request(wire::EXTENDED, &fields)
    }

    /// Removes a file or symbolic link, without recursing into directories.
    pub fn remove_file(&mut self, path: impl AsRef<[u8]>) -> Result<(), Error> {
        self.status_request(wire::REMOVE, &path_fields(path.as_ref(), false)?)
    }

    /// Removes an empty directory.
    pub fn remove_dir(&mut self, path: impl AsRef<[u8]>) -> Result<(), Error> {
        self.status_request(wire::RMDIR, &path_fields(path.as_ref(), false)?)
    }

    /// Sets POSIX mode bits (the low twelve bits) on an existing server path.
    pub fn set_permissions(&mut self, path: impl AsRef<[u8]>, mode: u32) -> Result<(), Error> {
        let mut fields = path_fields(path.as_ref(), false)?;
        push_permissions(&mut fields, mode);
        self.status_request(wire::SETSTAT, &fields)
    }

    /// Returns whether the server advertised version 1 of `lsetstat@openssh.com`.
    pub fn supports_lsetstat(&self) -> bool {
        self.lsetstat
    }

    /// Sets only POSIX mode bits using OpenSSH's no-follow metadata request.
    pub fn set_permissions_no_follow(
        &mut self,
        path: impl AsRef<[u8]>,
        mode: u32,
    ) -> Result<(), Error> {
        if !self.supports_lsetstat() {
            return Err(Error::Unsupported(
                "No-follow permission changes were not advertised",
            ));
        }
        if mode > 0o7777 {
            return Err(Error::InvalidInput("Expected POSIX permission bits"));
        }
        let mut fields = Vec::new();
        wire::string(&mut fields, LSETSTAT)?;
        push_path(&mut fields, path.as_ref(), false)?;
        push_permissions(&mut fields, mode);
        self.status_request(wire::EXTENDED, &fields)
    }

    /// Sets numeric ownership with OpenSSH's no-follow ownership request.
    /// Omitted IDs use the POSIX `chown` sentinel and retain that field.
    pub fn set_owner_no_follow(
        &mut self,
        path: impl AsRef<[u8]>,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> Result<(), Error> {
        if !self.supports_lsetstat() {
            return Err(Error::Unsupported(
                "No-follow ownership changes were not advertised",
            ));
        }
        if (uid.is_none() && gid.is_none()) || uid == Some(u32::MAX) || gid == Some(u32::MAX) {
            return Err(Error::InvalidInput(
                "Expected a numeric UID or GID below 4294967295",
            ));
        }
        let mut fields = Vec::new();
        wire::string(&mut fields, LSETSTAT)?;
        push_path(&mut fields, path.as_ref(), false)?;
        fields.extend_from_slice(&wire::ATTR_UID_GID.to_be_bytes());
        fields.extend_from_slice(&uid.unwrap_or(u32::MAX).to_be_bytes());
        fields.extend_from_slice(&gid.unwrap_or(u32::MAX).to_be_bytes());
        self.status_request(wire::EXTENDED, &fields)
    }

    fn read_directory(
        &mut self,
        handle: &[u8],
        max_entries: usize,
        max_bytes: usize,
    ) -> Result<Vec<DirectoryEntry>, Error> {
        let fields = handle_fields(handle)?;
        let mut entries = Vec::new();
        let mut total_entries = 0;
        let mut total_bytes = 0;
        loop {
            let result = self.request(wire::READDIR, &fields, wire::NAME, |decoder| {
                total_bytes += decoder.length();
                let count = decoder.u32()? as usize;
                if count == 0 || count > decoder.length() / 12 {
                    return Err(Error::Protocol("invalid SFTP READDIR entry count"));
                }
                total_entries += count;
                if total_entries > max_entries || total_bytes > max_bytes {
                    return Err(Error::Limit("directory listing"));
                }
                for _ in 0..count {
                    let entry = decode_entry(decoder)?;
                    match entry.name.as_slice() {
                        b"." | b".." => continue,
                        name if name.is_empty() || name.contains(&0) || name.contains(&b'/') => {
                            return Err(Error::Protocol("invalid SFTP directory entry name"));
                        }
                        _ => entries.push(entry),
                    }
                }
                Ok(())
            });
            if is_eof(&result) {
                return Ok(entries);
            }
            result?;
        }
    }

    fn handle_attributes(&mut self, handle: &[u8]) -> Result<Attributes, Error> {
        let fields = handle_fields(handle)?;
        self.request(wire::FSTAT, &fields, wire::ATTRS, |decoder| {
            decoder.attributes()
        })
    }

    fn check_download_overlap(
        &mut self,
        handle: &[u8],
        offset: u64,
        mut overlap: &[u8],
    ) -> Result<(), Error> {
        let mut offset = offset - overlap.len() as u64;
        let mut fields = handle_fields(handle)?;
        let handle_end = fields.len();
        while !overlap.is_empty() {
            fields.truncate(handle_end);
            let count =
                self.read_download_chunk(&mut fields, offset, overlap.len().min(CHUNK), |bytes| {
                    if !overlap.starts_with(bytes) {
                        return Err(Error::SourceChanged);
                    }
                    Ok(())
                });
            if is_eof(&count) {
                return Err(Error::SourceChanged);
            }
            let count = count?;
            offset += count as u64;
            overlap = &overlap[count..];
        }
        Ok(())
    }

    fn copy_download(
        &mut self,
        handle: &[u8],
        mut offset: u64,
        expected_size: Option<u64>,
        destination: &mut impl Write,
    ) -> Result<u64, Error> {
        let mut fields = handle_fields(handle)?;
        let handle_end = fields.len();
        loop {
            fields.truncate(handle_end);
            let result = self.read_download_chunk(&mut fields, offset, CHUNK, |bytes| {
                if expected_size
                    .is_some_and(|size| bytes.len() as u64 > size.saturating_sub(offset))
                {
                    return Err(Error::SourceChanged);
                }
                let mut remaining = bytes;
                while !remaining.is_empty() {
                    // Local transfer adapters use Interrupted to cancel. Unlike
                    // Write::write_all, this loop retries only after progress.
                    let count = destination.write(remaining).map_err(Error::LocalIo)?;
                    if count == 0 {
                        return Err(Error::LocalIo(io::ErrorKind::WriteZero.into()));
                    }
                    remaining = remaining.get(count..).ok_or_else(|| {
                        Error::LocalIo(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "download writer returned an invalid byte count",
                        ))
                    })?;
                }
                Ok(())
            });
            if is_eof(&result) {
                return Ok(offset);
            }
            offset = offset
                .checked_add(result? as u64)
                .ok_or(Error::Limit("download file offset"))?;
        }
    }

    fn read_download_chunk(
        &mut self,
        fields: &mut Vec<u8>,
        offset: u64,
        length: usize,
        consume: impl FnOnce(&[u8]) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        fields.extend_from_slice(&offset.to_be_bytes());
        fields.extend_from_slice(&(length as u32).to_be_bytes());
        self.request(wire::READ, fields, wire::DATA, |decoder| {
            let bytes = decoder.string()?;
            if bytes.is_empty() || bytes.len() > length {
                return Err(Error::Protocol("invalid SFTP READ data length"));
            }
            if decoder.length() != 0 {
                return Err(Error::Protocol("trailing bytes in SFTP READ response"));
            }
            consume(bytes)?;
            Ok(bytes.len())
        })
    }

    fn copy_upload(&mut self, handle: &[u8], source: &mut impl Read) -> Result<u64, Error> {
        let mut buffer = vec![0; CHUNK];
        let mut offset = 0u64;
        let mut fields = handle_fields(handle)?;
        let handle_end = fields.len();
        loop {
            let count = source.read(&mut buffer).map_err(Error::LocalIo)?;
            if count == 0 {
                return Ok(offset);
            }
            let end = offset
                .checked_add(count as u64)
                .ok_or(Error::Limit("upload file offset"))?;
            fields.truncate(handle_end);
            fields.extend_from_slice(&offset.to_be_bytes());
            let data = buffer.get(..count).ok_or_else(|| {
                Error::LocalIo(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "upload reader returned an invalid byte count",
                ))
            })?;
            wire::string(&mut fields, data)?;
            self.status_request(wire::WRITE, &fields)?;
            offset = end;
        }
    }

    fn with_handle<T>(
        &mut self,
        handle: &[u8],
        operation: impl FnOnce(&mut Self, &[u8]) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let result = operation(self, handle);
        let closed = self.status_request(wire::CLOSE, &handle_fields(handle)?);
        match (result, closed) {
            (Err(Error::SourceChanged), _) => Err(Error::SourceChanged),
            (Err(error), _) if error.connection_lost() => Err(error),
            (_, Err(error)) if error.connection_lost() => Err(error),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }

    fn open_handle(&mut self, kind: u8, fields: &[u8]) -> Result<Vec<u8>, Error> {
        self.request(kind, fields, wire::HANDLE, |decoder| {
            let handle = decoder.string()?;
            if handle.len() > MAX_HANDLE {
                return Err(Error::Protocol("invalid SFTP file handle length"));
            }
            Ok(handle.to_vec())
        })
    }

    fn open_read_file(&mut self, path: &[u8]) -> Result<Vec<u8>, Error> {
        let mut fields = path_fields(path, false)?;
        fields.extend_from_slice(&OPEN_READ.to_be_bytes());
        fields.extend_from_slice(&0u32.to_be_bytes());
        self.open_handle(wire::OPEN, &fields)
    }

    fn open_new_private_file(&mut self, path: &[u8]) -> Result<Vec<u8>, Error> {
        let mut fields = path_fields(path, false)?;
        fields.extend_from_slice(&(OPEN_WRITE | OPEN_CREATE | OPEN_EXCLUSIVE).to_be_bytes());
        push_permissions(&mut fields, 0o600);
        self.open_handle(wire::OPEN, &fields)
    }

    fn path_attributes(&mut self, kind: u8, path: &[u8]) -> Result<Attributes, Error> {
        let fields = path_fields(path, true)?;
        self.request(kind, &fields, wire::ATTRS, |decoder| decoder.attributes())
    }

    fn status_request(&mut self, kind: u8, fields: &[u8]) -> Result<(), Error> {
        self.request(kind, fields, wire::STATUS, |_| Ok(()))
    }

    fn request<T>(
        &mut self,
        kind: u8,
        fields: &[u8],
        expected: u8,
        decode: impl FnOnce(&mut Decoder<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let result = self.exchange(kind, fields, expected, decode);
        if result.as_ref().is_err_and(|error| error.connection_lost()) {
            self.failed = true;
        }
        result
    }

    fn exchange<T>(
        &mut self,
        kind: u8,
        fields: &[u8],
        expected: u8,
        decode: impl FnOnce(&mut Decoder<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if self.failed {
            return Err(Error::Closed);
        }
        let id = self.next_id;
        self.next_id = id.wrapping_add(1);
        let mut packet = Vec::with_capacity(5 + fields.len());
        packet.push(kind);
        packet.extend_from_slice(&id.to_be_bytes());
        packet.extend_from_slice(fields);
        wire::send(&mut self.writer, &packet)?;

        let reply = wire::receive(&mut self.reader)?;
        let mut decoder = Decoder::new(&reply);
        let response = decoder.byte()?;
        if decoder.u32()? != id {
            return Err(Error::Protocol("SFTP response request ID mismatch"));
        }
        if response == wire::STATUS {
            let status = decoder.status()?;
            if decoder.length() != 0 {
                return Err(Error::Protocol("trailing bytes in SFTP status response"));
            }
            if status.code != StatusCode::Ok {
                return Err(Error::Status(status));
            }
        }
        if response != expected {
            return Err(Error::Protocol("unexpected SFTP response type"));
        }
        let value = decode(&mut decoder)?;
        decoder.finish()?;
        Ok(value)
    }
}

fn is_eof<T>(result: &Result<T, Error>) -> bool {
    matches!(result, Err(Error::Status(status)) if status.code == StatusCode::Eof)
}

fn check_regular(expected: Attributes, current: Attributes) -> Result<(), Error> {
    if expected.kind() == FileKind::File
        && expected.without_access_time() == current.without_access_time()
    {
        Ok(())
    } else {
        Err(Error::SourceChanged)
    }
}

fn check_regular_handle(
    path: &[u8],
    expected: Attributes,
    mut opened: Attributes,
) -> Result<(), Error> {
    // Win32 OpenSSH 10.4p1 synthesizes different permission bits for LSTAT
    // and FSTAT. Match only the observed file modes on explicit drive paths.
    // Path checks and the subsequent same-handle comparison stay exact.
    let windows_modes = matches!(
        (expected.permissions, opened.permissions),
        (Some(0o100600 | 0o100700), Some(0o100666)) | (Some(0o100400 | 0o100500), Some(0o100444))
    );
    if windows_modes
        && expected.uid == Some(0)
        && expected.gid == Some(0)
        && expected.size.is_some()
        && expected.modified.is_some()
        && windows_drive_file(path)
    {
        opened.permissions = expected.permissions;
    }
    check_regular(expected, opened)
}

fn windows_drive_file(path: &[u8]) -> bool {
    let [b'/', letter, b':', b'/', rest @ ..] = path else {
        return false;
    };
    letter.is_ascii_alphabetic()
        && !rest.contains(&b':')
        && !rest.contains(&b'\\')
        && rest
            .split(|&byte| byte == b'/')
            .all(|component| !component.is_empty() && !matches!(component, b"." | b".."))
}

fn check_copy_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), Error> {
    if cancelled() {
        Err(Error::LocalIo(io::Error::from(io::ErrorKind::Interrupted)))
    } else {
        Ok(())
    }
}

fn decode_entry(decoder: &mut Decoder<'_>) -> Result<DirectoryEntry, Error> {
    let name = decoder.string()?;
    if name.len() > MAX_PATH {
        return Err(Error::Protocol(
            "SFTP response path exceeds the length limit",
        ));
    }
    decoder.string()?; // The server's display-only long name is not trusted as metadata.
    let attrs = decoder.attributes()?;
    Ok(DirectoryEntry {
        name: name.to_vec(),
        attrs,
    })
}

fn path_fields(path: &[u8], allow_empty: bool) -> Result<Vec<u8>, Error> {
    let mut fields = Vec::new();
    push_path(&mut fields, path, allow_empty)?;
    Ok(fields)
}

fn push_path(fields: &mut Vec<u8>, path: &[u8], allow_empty: bool) -> Result<(), Error> {
    if path.len() > MAX_PATH || path.contains(&0) || (!allow_empty && path.is_empty()) {
        return Err(Error::InvalidInput("invalid SFTP path"));
    }
    wire::string(fields, path)
}

fn handle_fields(handle: &[u8]) -> Result<Vec<u8>, Error> {
    let mut fields = Vec::new();
    wire::string(&mut fields, handle)?;
    Ok(fields)
}

fn push_permissions(fields: &mut Vec<u8>, mode: u32) {
    fields.extend_from_slice(&wire::ATTR_PERMISSIONS.to_be_bytes());
    fields.extend_from_slice(&(mode & 0o7777).to_be_bytes());
}

fn rename_fields(from: &[u8], to: &[u8], extension: Option<&[u8]>) -> Result<Vec<u8>, Error> {
    let mut fields = Vec::new();
    if let Some(extension) = extension {
        wire::string(&mut fields, extension)?;
    }
    push_path(&mut fields, from, false)?;
    push_path(&mut fields, to, false)?;
    Ok(fields)
}

#[cfg(test)]
mod tests;
