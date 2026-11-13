//! Handle-relative local input and publication for a single transfer.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::private_files::{FileStamp, require_single_link};
use crate::sftp_window::files::{LocalStamp, nonce};

use super::local::{self, Directory, Entry};

const PAYLOAD: &str = "payload";

mod ordinary;
mod resume;
mod upload_resume;
pub(crate) use ordinary::OrdinaryDownload;
pub(in crate::sftp_window) use upload_resume::UploadSource;

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

    pub(in crate::sftp_window) fn parent_identity(
        &self,
    ) -> io::Result<crate::sftp_window::worker::resume::persist::Identity> {
        crate::sftp_window::worker::resume::persist::Identity::directory(&self.parent)
    }

    pub(in crate::sftp_window) fn verify_reviewed(
        &self,
        stamp: &LocalStamp,
        identity: &crate::sftp_window::worker::resume::persist::Identity,
        parent: &crate::sftp_window::worker::resume::persist::Identity,
    ) -> io::Result<()> {
        use crate::sftp_window::worker::resume::persist::Identity;

        self.verify()?;
        if self.stamp != *stamp || Identity::directory(&self.parent)? != *parent {
            return Err(io::Error::other("The reviewed local source changed"));
        }
        let (_, name) = components(&self.path)?;
        let Entry::File(current) = local::entry(&self.parent, name)? else {
            return Err(io::Error::other("The reviewed local source changed"));
        };
        identity.require(&self.file)?;
        identity.require(&current)
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
    keep: bool,
    locked: bool,
    restore_private_access: bool,
}

impl StagedDownload {
    pub(crate) fn create(destination: &Path) -> io::Result<Self> {
        Self::create_with(destination, None, true, Directory::create_file)
    }

    pub(in crate::sftp_window) fn resumable(destination: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        let create = Directory::create_resumable_download_file;
        #[cfg(unix)]
        let create = Directory::create_read_write_file;
        let mut staged = Self::create_with(destination, None, true, create)?;
        staged.restore_private_access = true;
        Ok(staged)
    }

    pub(in crate::sftp_window) fn create_reviewed(
        destination: &Path,
        parent: &crate::sftp_window::worker::resume::persist::Identity,
    ) -> io::Result<Self> {
        Self::create_with(destination, Some(parent), true, Directory::create_file)
    }

    pub(in crate::sftp_window) fn stage_for_replacement(
        destination: &Path,
        parent: &crate::sftp_window::worker::resume::persist::Identity,
    ) -> io::Result<Self> {
        Self::stage_with_final_permissions(destination, parent, false)
    }

    pub(in crate::sftp_window) fn stage_new_download(
        destination: &Path,
        parent: &crate::sftp_window::worker::resume::persist::Identity,
    ) -> io::Result<Self> {
        Self::stage_with_final_permissions(destination, parent, true)
    }

    fn stage_with_final_permissions(
        destination: &Path,
        parent: &crate::sftp_window::worker::resume::persist::Identity,
        require_vacant: bool,
    ) -> io::Result<Self> {
        #[cfg(windows)]
        let create = Directory::create_replacement_file;
        #[cfg(unix)]
        let create = Directory::create_file;
        let mut staged = Self::create_with(destination, Some(parent), require_vacant, create)?;
        staged.restore_private_access = true;
        Ok(staged)
    }

    fn create_with(
        destination: &Path,
        reviewed_parent: Option<&crate::sftp_window::worker::resume::persist::Identity>,
        require_vacant: bool,
        create: impl FnOnce(&Directory, &OsStr) -> io::Result<File>,
    ) -> io::Result<Self> {
        let (parent, _) = components(destination)?;
        let parent = Directory::open_path(parent)?;
        if let Some(reviewed) = reviewed_parent {
            reviewed.require_directory(&parent)?;
        }
        if require_vacant {
            require_absent(destination)?;
        }
        let name = OsString::from(nonce()?);
        let stage = parent.create_child(&name)?;
        let file = match create(&stage, OsStr::new(PAYLOAD)) {
            Ok(file) => file,
            Err(error) => {
                let _ = local::discard(&parent, &name, stage);
                return Err(error);
            }
        };
        let download = Self {
            destination: destination.to_path_buf(),
            parent,
            name,
            stage: Some(stage),
            file: Some(file),
            keep: false,
            locked: false,
            restore_private_access: false,
        };
        #[cfg(target_os = "macos")]
        download.require_no_extended_acl()?;
        Ok(download)
    }

    pub(crate) fn writer(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("The staged download has no file"))
    }

    pub(in crate::sftp_window) fn parent_identity(
        &self,
    ) -> io::Result<crate::sftp_window::worker::resume::persist::Identity> {
        self.require_parent()?;
        crate::sftp_window::worker::resume::persist::Identity::directory(&self.parent)
    }

    pub(in crate::sftp_window) fn verify_reviewed_parent(
        &self,
        identity: &crate::sftp_window::worker::resume::persist::Identity,
    ) -> io::Result<()> {
        self.require_parent()?;
        if crate::sftp_window::worker::resume::persist::Identity::directory(&self.parent)?
            != *identity
        {
            return Err(io::Error::other(
                "The reviewed local destination folder changed",
            ));
        }
        Ok(())
    }

    pub(in crate::sftp_window) fn verify_partial(&self, stamp: &FileStamp) -> io::Result<()> {
        self.verify_stage(stamp)?;
        require_absent(&self.destination)
    }

    pub(in crate::sftp_window) fn verify_stage(&self, stamp: &FileStamp) -> io::Result<()> {
        self.require_parent()?;
        let file = self.writer()?;
        let stage = self
            .stage
            .as_ref()
            .ok_or_else(|| io::Error::other("Missing download stage"))?;
        let Entry::Directory(current) = local::entry(&self.parent, &self.name)? else {
            return Err(io::Error::other("The partial download directory changed"));
        };
        if !stage.same_as(&current)? {
            return Err(io::Error::other("The partial download directory changed"));
        }
        #[cfg(windows)]
        {
            stage.require_owner_only()?;
            Directory::require_owner_only_file(file)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if (stage.file().metadata()?.mode() | file.metadata()?.mode()) & 0o077 != 0 {
                return Err(io::Error::other(
                    "The partial download is no longer private",
                ));
            }
        }
        #[cfg(target_os = "macos")]
        self.require_no_extended_acl()?;
        require_single_link(file)?;
        stamp.require_unchanged(file)?;
        let Entry::File(current) = local::entry(stage, OsStr::new(PAYLOAD))? else {
            return Err(io::Error::other("The partial download file changed"));
        };
        stamp.require_unchanged(&current)
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
        self.publish_checked(|_| {
            if cancelled() {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                Ok(())
            }
        })
    }

    pub(in crate::sftp_window) fn publish_checked(
        mut self,
        before_publish: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        let file = self.writer()?;
        file.sync_all()?;
        let name = self.require_parent()?;
        let stage = self
            .stage
            .as_ref()
            .ok_or_else(|| io::Error::other("The staged download has no directory"))?;
        before_publish(file)?;
        self.require_parent()?;
        let Entry::Directory(current) = local::entry(&self.parent, &self.name)? else {
            return Err(io::Error::other("The local staging directory changed"));
        };
        if !stage.same_as(&current)? {
            return Err(io::Error::other("The local staging directory changed"));
        }
        local::publish_file(&self.parent, stage, OsStr::new(PAYLOAD), file, name)?;
        self.restore_private_access = false;
        self.keep = false;
        Ok(())
    }

    fn require_parent(&self) -> io::Result<&OsStr> {
        let (parent, name) = components(&self.destination)?;
        if !self.parent.same_as(&Directory::open_path(parent)?)? {
            return Err(io::Error::other("The local download parent changed"));
        }
        Ok(name)
    }
}

impl Drop for StagedDownload {
    fn drop(&mut self) {
        if self.restore_private_access
            && let Some(file) = &self.file
        {
            #[cfg(unix)]
            let _ = rustix::fs::fchmod(file, rustix::fs::Mode::from_raw_mode(0o600));
            #[cfg(target_os = "macos")]
            let _ = kortty_files::clear_extended_acl(file);
            #[cfg(windows)]
            let _ = kortty_files::restore_private_file_access(file);
        }
        if self.locked
            && let Some(file) = &self.file
        {
            let _ = file.unlock();
        }
        drop(self.file.take());
        if let Some(stage) = self.stage.take().filter(|_| !self.keep) {
            let _ = local::discard(&self.parent, &self.name, stage);
        }
    }
}

fn components(path: &Path) -> io::Result<(&Path, &OsStr)> {
    path.parent()
        .zip(path.file_name())
        .ok_or_else(|| io::Error::other("Expected a complete local file path"))
}

fn require_absent(destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The local download destination already exists",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

    #[test]
    fn reviewed_replacement_stage_preserves_the_original_and_requires_vacant_publication() {
        use crate::sftp_window::worker::resume::persist::Identity;

        let fixture = crate::snippets::tests::Profile::new();
        let parent = fixture.0.canonicalize().unwrap();
        let destination = parent.join("existing");
        fs::write(&destination, b"original").unwrap();
        let identity = Identity::directory(&Directory::open_path(&parent).unwrap()).unwrap();
        let stage = StagedDownload::stage_for_replacement(&destination, &identity).unwrap();
        stage.writer().unwrap().write_all(b"replacement").unwrap();
        let stamp = FileStamp::read(stage.writer().unwrap()).unwrap();
        stage.verify_stage(&stamp).unwrap();
        assert!(stage.verify_partial(&stamp).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert!(
            stage
                .publish_checked(|file| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        file.set_permissions(fs::Permissions::from_mode(0o0))?;
                    }
                    #[cfg(windows)]
                    kortty_files::ReplacementMetadata::capture(&File::open(&destination)?)?
                        .apply_to(file)?;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"original");
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
