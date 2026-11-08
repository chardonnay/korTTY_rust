//! Handle-relative operations for private files in the local profile.

use std::fs::File;
use std::io;
use std::time::SystemTime;

pub(crate) mod overwrite;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::Directory;
#[cfg(windows)]
pub(crate) use windows::Directory;

#[cfg(unix)]
pub(crate) type EntryIdentity = (u64, u64);
#[cfg(windows)]
pub(crate) type EntryIdentity = (u64, [u8; 16]);

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

    #[cfg(unix)]
    pub(crate) fn stamp_after_move(&self, file: &File) -> io::Result<Self> {
        let moved = Self::read(file)?;
        // Renaming can change ctime, so compare the immutable identity and content metadata.
        if (self.identity.0, self.identity.1, self.modified, self.length)
            != (
                moved.identity.0,
                moved.identity.1,
                moved.modified,
                moved.length,
            )
        {
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
