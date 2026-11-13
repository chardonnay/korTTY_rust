//! Handle-relative operations for private files in the local profile.

use std::fs::File;
use std::io;
use std::ops::Deref;
use std::time::SystemTime;

pub(crate) mod batch;
pub(crate) mod desktop;
pub(crate) mod executable;
pub(crate) mod overwrite;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::{Directory, require_administrator_controlled};
#[cfg(windows)]
pub(crate) use windows::Directory;

#[cfg(unix)]
pub(crate) type EntryIdentity = (u64, u64);
#[cfg(windows)]
pub(crate) type EntryIdentity = (u64, [u8; 16]);

/// A held file lock whose owner releases it before closing the descriptor.
#[derive(Debug)]
pub(crate) struct FileLock(File);

impl FileLock {
    /// Takes ownership of a file with an acquired lock.
    pub(crate) fn new(file: File) -> Self {
        Self(file)
    }
}

impl Deref for FileLock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // A forked child can retain the descriptor until it execs.
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileStamp {
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    #[cfg(windows)]
    identity: (u64, [u8; 16]),
    pub(crate) modified: SystemTime,
    length: u64,
}

impl FileStamp {
    pub(crate) fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid("entry is not a regular file"));
        }
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;

            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        };
        #[cfg(windows)]
        let identity = {
            if kortty_files::tree::is_reparse_point(&metadata) {
                return Err(invalid("entry is a reparse point"));
            }
            kortty_files::tree::file_identity(file)?
        };
        Ok(Self {
            identity,
            modified: metadata.modified()?,
            length: metadata.len(),
        })
    }

    pub(crate) fn require_unchanged(&self, file: &File) -> io::Result<()> {
        if Self::read(file)? != *self {
            return Err(invalid("file changed before the operation could finish"));
        }
        Ok(())
    }

    pub(crate) fn append_identity(&self, bytes: &mut Vec<u8>) {
        bytes.extend(self.identity.0.to_le_bytes());
        #[cfg(unix)]
        {
            bytes.extend(self.identity.1.to_le_bytes());
            bytes.extend(self.identity.2.to_le_bytes());
            bytes.extend(self.identity.3.to_le_bytes());
        }
        #[cfg(windows)]
        bytes.extend(self.identity.1);
        let (before_epoch, duration) = match self.modified.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => (false, duration),
            Err(error) => (true, error.duration()),
        };
        bytes.push(u8::from(before_epoch));
        bytes.extend(duration.as_secs().to_le_bytes());
        bytes.extend(duration.subsec_nanos().to_le_bytes());
        bytes.extend(self.length.to_le_bytes());
    }

    pub(crate) fn stamp_after_move(&self, file: &File) -> io::Result<Self> {
        let moved = Self::read(file)?;
        let same_file = (self.identity.0, self.identity.1, self.length)
            == (moved.identity.0, moved.identity.1, moved.length);
        // Rename changes ctime on Unix. On Windows, mtime may settle when the writer closes.
        #[cfg(unix)]
        let same_file = same_file && self.modified == moved.modified;
        if !same_file {
            return Err(invalid("file changed before the operation could finish"));
        }
        Ok(moved)
    }
}

pub(crate) fn nonce() -> io::Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

pub(crate) fn require_single_link(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    let links = {
        use std::os::unix::fs::MetadataExt as _;
        file.metadata()?.nlink()
    };
    #[cfg(windows)]
    let links = kortty_files::tree::file_link_count(file)?;
    if links != 1 {
        return Err(invalid("The file must have one hard link"));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
