use std::fmt;
use std::io::{self, Read, Write};
use std::sync::Arc;

use super::{Attributes, Error, FileKind, OPEN_READ, Sftp, path_fields, wire};

pub const MAX_EDIT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
struct Version {
    path: Vec<u8>,
    attributes: Attributes,
    owner: Arc<()>,
}

/// Bounded text and the version of its regular file on one SFTP client.
#[derive(Clone)]
pub struct RemoteText {
    version: Version,
    text: Arc<str>,
}

impl RemoteText {
    pub fn path(&self) -> &[u8] {
        &self.version.path
    }

    pub fn text(&self) -> &Arc<str> {
        &self.text
    }
}

impl fmt::Debug for RemoteText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteText")
            .field("bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

/// Bounded file bytes and the version of its regular file on one SFTP client.
#[derive(Clone)]
pub struct RemoteFile {
    version: Version,
    bytes: Arc<[u8]>,
}

impl RemoteFile {
    pub fn path(&self) -> &[u8] {
        &self.version.path
    }

    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }
}

impl fmt::Debug for RemoteFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteFile")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl<R: Read, W: Write> Sftp<R, W> {
    /// Loads a regular UTF-8 file within a canonical directory.
    ///
    /// The final component is checked separately so the editor rejects a link
    /// rather than resolving it into a different file.
    pub fn open_text(&mut self, path: &[u8]) -> Result<RemoteText, Error> {
        let (version, bytes) = self.read_file_for_edit(path)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::InvalidInput("The remote file is not UTF-8"))?;
        validate_text(&text)?;
        Ok(RemoteText {
            version,
            text: Arc::from(text),
        })
    }

    /// Loads a regular file for an external editor without interpreting its bytes.
    pub fn open_external_file(&mut self, path: &[u8]) -> Result<RemoteFile, Error> {
        let (version, bytes) = self.read_file_for_edit(path)?;
        Ok(RemoteFile {
            version,
            bytes: Arc::from(bytes),
        })
    }

    /// Replaces an opened file after checking its current metadata and exact bytes.
    ///
    /// SFTP v3 provides neither a no-follow open nor a conditional rename. The
    /// metadata and byte checks detect observed changes before atomic replacement.
    /// The new inode preserves the reported owner, group and mode. SFTP v3
    /// does not provide a portable way to retain ACLs or extended attributes.
    pub fn save_text(
        &mut self,
        opened: &RemoteText,
        text: Arc<str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<RemoteText, Error> {
        self.save_checked_text(opened, text, false, cancelled)
    }

    /// Saves an external text edit with the remote parent protection.
    pub fn save_external_text(
        &mut self,
        opened: &RemoteText,
        text: Arc<str>,
        cancelled: impl Fn() -> bool,
    ) -> Result<RemoteText, Error> {
        self.save_checked_text(opened, text, true, cancelled)
    }

    fn save_checked_text(
        &mut self,
        opened: &RemoteText,
        text: Arc<str>,
        protect_parent: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<RemoteText, Error> {
        validate_text(&text)?;
        let version = self.save_checked_file(
            &opened.version,
            opened.text.as_bytes(),
            text.as_bytes(),
            protect_parent,
            cancelled,
        )?;
        Ok(RemoteText { version, text })
    }

    /// Saves an external edit with additional protection against other writers
    /// replacing staging paths in a shared remote directory.
    pub fn save_external_file(
        &mut self,
        opened: &RemoteFile,
        bytes: Arc<[u8]>,
        cancelled: impl Fn() -> bool,
    ) -> Result<RemoteFile, Error> {
        let version =
            self.save_checked_file(&opened.version, &opened.bytes, &bytes, true, cancelled)?;
        Ok(RemoteFile { version, bytes })
    }

    fn save_checked_file(
        &mut self,
        opened: &Version,
        original: &[u8],
        bytes: &[u8],
        protect_parent: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Version, Error> {
        if !Arc::ptr_eq(&self.identity, &opened.owner) {
            return Err(Error::EditConflict(
                "The SFTP connection changed. Reopen the file.",
            ));
        }
        if bytes.len() > MAX_EDIT_BYTES {
            return Err(Error::Limit("remote editor file size"));
        }
        check_cancelled(&cancelled)?;
        self.check_parent(&opened.path)?;
        let current = self.lstat(&opened.path)?;
        require_same(opened.attributes, current)?;
        if bytes == original {
            self.check_version(opened, original)?;
            return Ok(opened.clone());
        }
        if !self.supports_atomic_replace() {
            return Err(Error::Unsupported(
                "This server does not support atomic file replacement",
            ));
        }
        let mode = current
            .permissions
            .filter(|mode| mode & 0o7000 == 0)
            .ok_or(Error::Unsupported(
                "Cannot preserve the file's permission bits",
            ))?
            & 0o777;
        if protect_parent && mode & 0o022 != 0 {
            return Err(Error::Unsupported(
                "External editor upload requires an owner-controlled remote file",
            ));
        }
        if current.uid.is_none() || current.gid.is_none() {
            return Err(Error::Unsupported(
                "Cannot verify the file's owner and group",
            ));
        }
        let (parent, _) = target_parts(&opened.path)?;
        if protect_parent {
            self.require_external_parent(parent, current)?;
        }
        let directory = child(parent, stage_name()?.as_bytes());
        self.mkdir_private(&directory)?;
        let stage = child(&directory, b"document");
        let result = (|| {
            self.require_private_stage(&directory, current)?;
            check_cancelled(&cancelled)?;
            let count = self.upload_new(&stage, bytes)?;
            let staged = self.lstat(&stage)?;
            if count != bytes.len() as u64
                || staged.size != Some(count)
                || staged.kind() != FileKind::File
                || staged.uid != current.uid
                || staged.gid != current.gid
                || staged
                    .permissions
                    .is_none_or(|value| value & 0o777 != 0o600)
            {
                return Err(Error::EditConflict(
                    "Could not verify private staging and file ownership",
                ));
            }
            self.set_permissions(&stage, mode)?;
            let staged = self.lstat(&stage)?;
            if staged.kind() != FileKind::File
                || staged.size != Some(count)
                || staged.uid != current.uid
                || staged.gid != current.gid
                || staged
                    .permissions
                    .is_none_or(|value| value & 0o7777 != mode)
            {
                return Err(Error::EditConflict(
                    "Could not preserve the remote file's permissions",
                ));
            }
            self.check_version(opened, original)?;
            self.require_private_stage(&directory, current)?;
            require_same(staged, self.lstat(&stage)?)?;
            require_same(opened.attributes, self.lstat(&opened.path)?)?;
            if protect_parent {
                self.require_external_parent(parent, current)?;
            }
            check_cancelled(&cancelled)?;
            self.replace(&stage, &opened.path).map_err(|error| {
                if error.connection_lost() {
                    Error::SaveUncertain(Box::new(error))
                } else {
                    error
                }
            })?;
            Ok(Version {
                attributes: staged.without_access_time(),
                ..opened.clone()
            })
        })();
        if !protect_parent || self.require_external_parent(parent, current).is_ok() {
            if result.is_err() && self.require_private_stage(&directory, current).is_ok() {
                let _ = self.remove_file(&stage);
            }
            let _ = self.remove_dir(&directory);
        }
        result
    }

    fn require_external_parent(
        &mut self,
        mut path: &[u8],
        original: Attributes,
    ) -> Result<(), Error> {
        for depth in 0..64 {
            let parent = self.lstat(path)?;
            if parent.kind() != FileKind::Directory
                || !(parent.uid == original.uid || (depth != 0 && parent.uid == Some(0)))
                || parent.permissions.is_none_or(|mode| mode & 0o022 != 0)
            {
                return Err(Error::Unsupported(
                    "External editor upload requires owner-controlled remote directories",
                ));
            }
            if path == b"/" {
                return Ok(());
            }
            (path, _) = target_parts(path)?;
        }
        Err(Error::Unsupported("The remote edit directory is too deep"))
    }

    fn read_file_for_edit(&mut self, path: &[u8]) -> Result<(Version, Vec<u8>), Error> {
        self.check_parent(path)?;
        let attributes = self.lstat(path)?.without_access_time();
        if attributes.kind() != FileKind::File {
            return Err(Error::InvalidInput(
                "Only regular remote files can be edited",
            ));
        }
        let size = attributes.size.ok_or(Error::Unsupported(
            "The server did not report the file size",
        ))?;
        if size > MAX_EDIT_BYTES as u64 {
            return Err(Error::Limit("remote editor file size"));
        }
        let mut fields = path_fields(path, false)?;
        fields.extend_from_slice(&OPEN_READ.to_be_bytes());
        fields.extend_from_slice(&0u32.to_be_bytes());
        let handle = self.open_handle(wire::OPEN, &fields)?;
        let mut bytes = FileBytes {
            contents: Vec::with_capacity(size as usize),
            limit: size as usize,
        };
        self.with_handle(&handle, |client, handle| {
            require_same(attributes, client.handle_attributes(handle)?)?;
            require_same(attributes, client.lstat(path)?)?;
            if client.copy_download(handle, 0, Some(size), &mut bytes)? != size {
                return Err(Error::EditConflict(
                    "The remote file changed during the read",
                ));
            }
            require_same(attributes, client.handle_attributes(handle)?)
        })?;
        self.check_parent(path)?;
        require_same(attributes, self.lstat(path)?)?;
        Ok((
            Version {
                path: path.to_vec(),
                attributes,
                owner: Arc::clone(&self.identity),
            },
            bytes.contents,
        ))
    }

    fn check_parent(&mut self, path: &[u8]) -> Result<(), Error> {
        let (parent, _) = target_parts(path)?;
        if self.canonicalize(parent)? != parent {
            return Err(Error::EditConflict("The remote parent directory changed"));
        }
        Ok(())
    }

    fn check_version(&mut self, opened: &Version, original: &[u8]) -> Result<(), Error> {
        let (version, bytes) = match self.read_file_for_edit(&opened.path) {
            Ok(current) => current,
            Err(error) if error.is_not_found() => {
                return Err(Error::EditConflict("The remote file was removed"));
            }
            Err(error) => return Err(error),
        };
        require_same(opened.attributes, version.attributes)?;
        if original != bytes {
            return Err(Error::EditConflict(
                "The remote file changed since it was opened",
            ));
        }
        Ok(())
    }

    fn require_private_stage(&mut self, path: &[u8], original: Attributes) -> Result<(), Error> {
        self.check_parent(path)?;
        let attributes = self.lstat(path)?;
        if attributes.kind() != FileKind::Directory
            || attributes
                .permissions
                .is_none_or(|mode| mode & 0o777 != 0o700)
            || attributes.uid != original.uid
            || attributes.gid != original.gid
        {
            return Err(Error::Unsupported(
                "Cannot verify private staging with the original owner and group",
            ));
        }
        Ok(())
    }
}

fn target_parts(path: &[u8]) -> Result<(&[u8], &[u8]), Error> {
    let slash = path.iter().rposition(|&byte| byte == b'/');
    if let Some(slash) = slash
        && path.starts_with(b"/")
        && !path.contains(&0)
        && let name = &path[slash + 1..]
        && !name.is_empty()
        && name != b"."
        && name != b".."
    {
        Ok((&path[..slash.max(1)], name))
    } else {
        Err(Error::InvalidInput("Expected an absolute remote file path"))
    }
}

fn child(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = parent.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

fn require_same(previous: Attributes, current: Attributes) -> Result<(), Error> {
    if previous.without_access_time() == current.without_access_time() {
        Ok(())
    } else {
        Err(Error::EditConflict(
            "The remote file changed since it was opened",
        ))
    }
}

fn validate_text(text: &str) -> Result<(), Error> {
    if text.len() > MAX_EDIT_BYTES {
        return Err(Error::Limit("remote editor file size"));
    }
    if text.contains('\0') {
        return Err(Error::InvalidInput("The remote file contains binary data"));
    }
    Ok(())
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), Error> {
    if cancelled() {
        Err(Error::Closed)
    } else {
        Ok(())
    }
}

fn stage_name() -> Result<String, Error> {
    use std::fmt::Write as _;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|error| Error::LocalIo(io::Error::other(error)))?;
    let mut name = String::from(".kortty-edit-");
    for byte in random {
        let _ = write!(name, "{byte:02x}");
    }
    Ok(name)
}

struct FileBytes {
    contents: Vec<u8>,
    limit: usize,
}

impl Write for FileBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.contents.len()) {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "remote file grew while being read",
            ));
        }
        self.contents.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
