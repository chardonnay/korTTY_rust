//! Reopens only the original private payload and verifies its complete saved prefix.

use std::io::{Read as _, Seek as _, SeekFrom};

use pgp::crypto::hash::HashAlgorithm;

use crate::sftp_window::worker::resume::persist::{Identity, LocalPath, Payload};

use super::*;

mod completed;

impl StagedDownload {
    pub(in crate::sftp_window) fn checkpoint(
        &mut self,
        cancelled: impl Fn() -> bool,
    ) -> io::Result<Payload> {
        self.require_parent()?;
        let identity = Identity::capture(self.writer()?)?;
        self.writer()?.sync_all()?;
        let stage = self
            .stage
            .as_ref()
            .ok_or_else(|| invalid("Missing download stage"))?;
        let file = self.writer()?;
        let offset = file.metadata()?.len();
        if offset == 0 || offset > 1024 * 1024 * 1024 {
            return Err(invalid("The download prefix exceeds the resume limits"));
        }
        let stamp = FileStamp::read(file)?;
        self.verify_partial(&stamp)?;
        let sha256 = prefix(file, offset, &cancelled)?;
        self.verify_partial(&stamp)?;
        Ok(Payload {
            destination: LocalPath::capture(&self.destination),
            stage_name: self
                .name
                .to_str()
                .ok_or_else(|| invalid("Invalid stage name"))?
                .into(),
            parent: Identity::directory(&self.parent)?,
            stage: Identity::directory(stage)?,
            file: identity,
            offset,
            sha256,
        })
    }

    pub(in crate::sftp_window) fn reopen_checkpoint(
        saved: &Payload,
        cancelled: impl Fn() -> bool,
    ) -> io::Result<Self> {
        let destination = saved.destination.path()?;
        let (parent, _) = components(&destination)?;
        let parent = Directory::open_path(parent)?;
        saved.parent.require_directory(&parent)?;
        require_absent(&destination)?;
        let name = OsString::from(&saved.stage_name);
        let stage = reopen_stage(&parent, &name)?;
        saved.stage.require_directory(&stage)?;
        let file = reopen(&stage)?;
        saved.file.require(&file)?;
        let mut download = Self {
            destination,
            parent,
            name,
            stage: Some(stage),
            file: Some(file),
            keep: true,
            locked: false,
            restore_private_access: false,
        };
        download.lock_checkpoint()?;
        let stamp = FileStamp::read(download.writer()?)?;
        download.verify_partial(&stamp)?;
        if download.writer()?.metadata()?.len() < saved.offset
            || prefix(download.writer()?, saved.offset, &cancelled)? != saved.sha256
        {
            return Err(invalid("The saved download prefix changed"));
        }
        download.verify_partial(&stamp)?;
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        // A crash can leave bytes after the last durable checkpoint. They are
        // discarded only after verifying the saved prefix and payload identity.
        download.writer()?.set_len(saved.offset)?;
        download.writer()?.sync_all()?;
        let current = FileStamp::read(download.writer()?)?;
        download.verify_partial(&current)?;
        download.restore_private_access = true;
        Ok(download)
    }

    pub(in crate::sftp_window) fn lock_checkpoint(&mut self) -> io::Result<()> {
        if !self.locked {
            lock(self.writer()?)?;
            self.locked = true;
        }
        Ok(())
    }

    pub(in crate::sftp_window) fn keep_checkpoint(&mut self) {
        self.keep = true;
    }

    pub(in crate::sftp_window) fn discard_checkpoint(&mut self) {
        self.keep = false;
    }
}

#[cfg(unix)]
fn reopen_stage(parent: &Directory, name: &OsStr) -> io::Result<Directory> {
    let Entry::Directory(stage) = local::entry(parent, name)? else {
        return Err(invalid("The saved download stage is missing"));
    };
    Ok(stage)
}

#[cfg(windows)]
fn reopen_stage(parent: &Directory, name: &OsStr) -> io::Result<Directory> {
    Directory::from_file(parent.child_for_removal(name)?)
}

fn lock(file: &File) -> io::Result<()> {
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another transfer owns the saved download",
        ),
        std::fs::TryLockError::Error(error) => error,
    })
}

pub(super) fn prefix(
    file: &File,
    length: u64,
    cancelled: &impl Fn() -> bool,
) -> io::Result<String> {
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut hasher = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = vec![0; 64 * 1024];
    while remaining != 0 {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(invalid("The saved download prefix was truncated"));
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let mut digest = String::with_capacity(64);
    use std::fmt::Write as _;
    for byte in hasher.finalize() {
        write!(digest, "{byte:02x}").expect("writing into a string");
    }
    Ok(digest)
}

#[cfg(unix)]
fn reopen(directory: &Directory) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    Ok(File::from(openat(
        directory.file(),
        PAYLOAD,
        flags,
        Mode::empty(),
    )?))
}

#[cfg(windows)]
fn reopen(directory: &Directory) -> io::Result<File> {
    directory.open_resumable_download_file(OsStr::new(PAYLOAD))
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}
