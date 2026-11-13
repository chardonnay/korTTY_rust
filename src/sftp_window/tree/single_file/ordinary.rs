//! Scheduler files use native destination inheritance after private staging.

use super::*;
use crate::sftp_window::worker::ordinary_access::Access;
use crate::sftp_window::worker::resume::persist::Identity;

pub(crate) struct OrdinaryDownload {
    staged: StagedDownload,
    access: Access,
}

impl OrdinaryDownload {
    pub(crate) fn create(destination: &Path) -> io::Result<Self> {
        Self::create_checked(destination, None)
    }

    pub(in crate::sftp_window) fn create_reviewed(
        destination: &Path,
        expected: &Identity,
    ) -> io::Result<Self> {
        Self::create_checked(destination, Some(expected))
    }

    fn create_checked(destination: &Path, expected: Option<&Identity>) -> io::Result<Self> {
        let (parent, _) = components(destination)?;
        let directory = Directory::open_path(parent)?;
        let identity = Identity::directory(&directory)?;
        if expected.is_some_and(|expected| expected != &identity) {
            return Err(io::Error::other("The local destination folder changed"));
        }
        let access = Access::capture(parent, &identity)?;
        let staged = StagedDownload::stage_new_download(destination, &identity)?;
        Ok(Self { staged, access })
    }

    pub(crate) fn writer(&self) -> io::Result<&File> {
        self.staged.writer()
    }

    pub(crate) fn publish(self, cancelled: impl Fn() -> bool) -> io::Result<()> {
        self.publish_checked(|| {
            if cancelled() {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                Ok(())
            }
        })
    }

    pub(in crate::sftp_window) fn publish_checked(
        self,
        verify: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        self.staged.publish_checked(|file| {
            verify()?;
            self.access.apply_to(file)?;
            file.sync_all()?;
            self.access.verify_parent()?;
            verify()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn ordinary_scheduled_file_stays_private_then_matches_native_creation() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let control = File::create_new(root.join("control")).unwrap();
        let target = root.join("download");
        let stage = OrdinaryDownload::create(&target).unwrap();
        stage
            .writer()
            .unwrap()
            .write_all(b"scheduler bytes")
            .unwrap();
        assert!(!target.exists());
        #[cfg(windows)]
        Directory::require_owner_only_file(stage.writer().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                stage
                    .writer()
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        stage.publish(|| false).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"scheduler bytes");
        #[cfg(windows)]
        kortty_files::ReplacementMetadata::capture(&control)
            .unwrap()
            .require_matches(&File::open(&target).unwrap())
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode(),
                control.metadata().unwrap().permissions().mode()
            );
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn ordinary_scheduled_file_cancellation_and_collision_remove_the_stage() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let target = root.join("download");
        let stage = OrdinaryDownload::create(&target).unwrap();
        stage.writer().unwrap().write_all(b"private data").unwrap();
        assert!(stage.publish(|| true).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let stage = OrdinaryDownload::create(&target).unwrap();
        stage.writer().unwrap().write_all(b"private data").unwrap();
        fs::write(&target, b"concurrent data").unwrap();
        assert!(stage.publish(|| false).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"concurrent data");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }
}
