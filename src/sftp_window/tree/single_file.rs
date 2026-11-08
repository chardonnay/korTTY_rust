//! Handle-relative local input and publication for a single unattended transfer.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::sftp_window::files::{LocalStamp, nonce};

use super::local::{self, Directory, Entry};

const PAYLOAD: &str = "payload";

pub(crate) struct LocalSource {
    path: PathBuf,
    parent: Directory,
    file: File,
    stamp: LocalStamp,
}

impl LocalSource {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let (parent, name) = components(path)?;
        let parent = Directory::open_path(parent)?;
        let Entry::File(file) = local::entry(&parent, name)? else {
            return Err(io::Error::other("The local source must be a regular file"));
        };
        let stamp = LocalStamp::capture(&file.metadata()?);
        Ok(Self {
            path: path.to_path_buf(),
            parent,
            file,
            stamp,
        })
    }

    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    #[cfg(unix)]
    pub(crate) fn parent_file(&self) -> &File {
        self.parent.file()
    }

    #[cfg(windows)]
    pub(crate) fn parent_directory(&self) -> &kortty_files::tree::Directory {
        &self.parent
    }

    pub(crate) fn sibling_exists_or_uncertain(&self, name: &OsStr) -> bool {
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, statat};
            use rustix::io::Errno;
            Path::new(name).file_name() != Some(name)
                || !matches!(
                    statat(self.parent.file(), name, AtFlags::SYMLINK_NOFOLLOW),
                    Err(Errno::NOENT)
                )
        }
        #[cfg(windows)]
        !matches!(
            self.sibling(name),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        )
    }

    pub(crate) fn open_sibling(&self, name: &OsStr) -> io::Result<File> {
        match self.sibling(name)? {
            Entry::File(file) => Ok(file),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Expected a regular file beside the local source",
            )),
        }
    }

    fn sibling(&self, name: &OsStr) -> io::Result<Entry> {
        if Path::new(name).file_name() != Some(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected one sibling filename",
            ));
        }
        local::entry(&self.parent, name)
    }

    pub(crate) fn size(&self) -> u64 {
        self.stamp.size
    }

    pub(crate) fn verify(&self) -> io::Result<()> {
        let (parent, name) = components(&self.path)?;
        if self.parent.same_as(&Directory::open_path(parent)?)?
            && self.stamp == LocalStamp::capture(&self.file.metadata()?)
            && let Entry::File(current) = local::entry(&self.parent, name)?
            && self.stamp == LocalStamp::capture(&current.metadata()?)
        {
            return Ok(());
        }
        Err(io::Error::other("The local upload source changed"))
    }
}

pub(crate) struct StagedDownload {
    destination: PathBuf,
    parent: Directory,
    name: OsString,
    stage: Option<Directory>,
    file: Option<File>,
}

impl StagedDownload {
    pub(crate) fn create(destination: &Path) -> io::Result<Self> {
        let (parent, _) = components(destination)?;
        let parent = Directory::open_path(parent)?;
        match fs::symlink_metadata(destination) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The local download destination already exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let name = OsString::from(nonce()?);
        let stage = parent.create_child(&name)?;
        let file = match stage.create_file(OsStr::new(PAYLOAD)) {
            Ok(file) => file,
            Err(error) => {
                let _ = local::discard(&parent, &name, stage);
                return Err(error);
            }
        };
        Ok(Self {
            destination: destination.to_path_buf(),
            parent,
            name,
            stage: Some(stage),
            file: Some(file),
        })
    }

    pub(crate) fn writer(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("The staged download has no file"))
    }

    pub(crate) fn shares_parent_with(&self, source: &LocalSource) -> io::Result<bool> {
        self.parent.same_as(&source.parent)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn require_no_extended_acl(&self) -> io::Result<()> {
        let stage = self
            .stage
            .as_ref()
            .ok_or_else(|| io::Error::other("Missing staged directory"))?;
        kortty_files::require_no_extended_acl(stage.file())?;
        kortty_files::require_no_extended_acl(self.writer()?)
    }

    pub(crate) fn publish(self, cancelled: impl Fn() -> bool) -> io::Result<()> {
        let file = self.writer()?;
        file.sync_all()?;
        let (parent, name) = components(&self.destination)?;
        if !self.parent.same_as(&Directory::open_path(parent)?)? {
            return Err(io::Error::other("The local download parent changed"));
        }
        let stage = self
            .stage
            .as_ref()
            .ok_or_else(|| io::Error::other("The staged download has no directory"))?;
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        local::publish_file(&self.parent, stage, OsStr::new(PAYLOAD), file, name)
    }
}

impl Drop for StagedDownload {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Some(stage) = self.stage.take() {
            let _ = local::discard(&self.parent, &self.name, stage);
        }
    }
}

fn components(path: &Path) -> io::Result<(&Path, &OsStr)> {
    path.parent()
        .zip(path.file_name())
        .ok_or_else(|| io::Error::other("Expected a complete local file path"))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn scheduled_local_stage_publishes_only_to_a_new_path_and_cleans_on_cancel() {
        let fixture = crate::snippets::tests::Profile::new();
        let parent = fixture.0.canonicalize().unwrap();
        let destination = parent.join("download");
        let stage = StagedDownload::create(&destination).unwrap();
        stage.writer().unwrap().write_all(b"private data").unwrap();
        assert!(stage.publish(|| true).is_err());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);

        let stage = StagedDownload::create(&destination).unwrap();
        stage.writer().unwrap().write_all(b"private data").unwrap();
        fs::write(&destination, b"concurrent file").unwrap();
        assert!(stage.publish(|| false).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"concurrent file");
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
        fs::remove_file(&destination).unwrap();

        let stage = StagedDownload::create(&destination).unwrap();
        stage.writer().unwrap().write_all(b"private data").unwrap();
        stage.publish(|| false).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"private data");
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn scheduled_local_stage_refuses_a_replaced_parent_and_source_symlinks() {
        use std::os::unix::fs::symlink;

        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let original = root.join("original");
        let moved = root.join("moved");
        let replacement = root.join("replacement");
        fs::create_dir(&original).unwrap();
        fs::create_dir(&replacement).unwrap();
        let stage = StagedDownload::create(&original.join("download")).unwrap();
        stage.writer().unwrap().write_all(b"private data").unwrap();
        fs::rename(&original, &moved).unwrap();
        symlink(&replacement, &original).unwrap();
        assert!(stage.publish(|| false).is_err());
        assert_eq!(fs::read_dir(&replacement).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);

        let source = replacement.join("ordinary-file");
        fs::write(&source, b"payload").unwrap();
        assert!(LocalSource::open(&original.join("ordinary-file")).is_err());
        let alias = replacement.join("alias");
        symlink(&source, &alias).unwrap();
        assert!(LocalSource::open(&alias).is_err());
    }
}
