//! Reconciles publication that completed before its receipt could be removed.

use super::*;

impl StagedDownload {
    pub(in crate::sftp_window) fn checkpoint_missing(saved: &Payload) -> io::Result<bool> {
        let destination = saved.destination.path()?;
        let (parent, _) = components(&destination)?;
        let parent = Directory::open_path(parent)?;
        saved.parent.require_directory(&parent)?;
        let stage = match local::entry(&parent, OsStr::new(&saved.stage_name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Ok(Entry::Directory(stage)) => stage,
            _ => return Ok(false),
        };
        saved.stage.require_directory(&stage)?;
        Ok(matches!(local::entry(&stage, OsStr::new(PAYLOAD)),
            Err(error) if error.kind() == io::ErrorKind::NotFound))
    }

    pub(in crate::sftp_window) fn checkpoint_published(
        saved: &Payload,
        total: u64,
    ) -> io::Result<bool> {
        let destination = saved.destination.path()?;
        let (parent, name) = components(&destination)?;
        let parent = Directory::open_path(parent)?;
        saved.parent.require_directory(&parent)?;
        if !published_file_matches(&parent, name, &saved.file, total)? {
            return Ok(false);
        }
        // A crash between rename and Drop may leave the empty private stage.
        // Only an exact, empty stage is eligible for cleanup.
        if let Ok(stage) = reopen_stage(&parent, OsStr::new(&saved.stage_name))
            && saved.stage.require_directory(&stage).is_ok()
        {
            #[cfg(unix)]
            {
                let _ = rustix::fs::unlinkat(
                    parent.file(),
                    OsStr::new(&saved.stage_name),
                    rustix::fs::AtFlags::REMOVEDIR,
                );
            }
            #[cfg(windows)]
            {
                let _ = stage.remove();
            }
        }
        Ok(true)
    }
}

// Completion checks need metadata only: final permissions may deliberately
// deny reading file contents, including POSIX mode 000.
#[cfg(unix)]
fn published_file_matches(
    parent: &Directory,
    name: &OsStr,
    identity: &Identity,
    total: u64,
) -> io::Result<bool> {
    use rustix::fs::{AtFlags, FileType, statat};

    let metadata = match statat(parent.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(
        FileType::from_raw_mode(metadata.st_mode) == FileType::RegularFile
            && identity.matches_stat(&metadata)
            && i128::from(metadata.st_size) == i128::from(total),
    )
}

#[cfg(windows)]
fn published_file_matches(
    parent: &Directory,
    name: &OsStr,
    identity: &Identity,
    total: u64,
) -> io::Result<bool> {
    let file = match parent.open_file_metadata(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(identity.require(&file).is_ok() && file.metadata()?.len() == total)
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn completed_download_without_read_permission_cleans_its_exact_empty_stage() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let target = root.join("download");
        let mut download = StagedDownload::resumable(&target).unwrap();
        download
            .writer()
            .unwrap()
            .write_all(b"complete bytes")
            .unwrap();
        let saved = download.checkpoint(|| false).unwrap();
        download
            .writer()
            .unwrap()
            .set_permissions(fs::Permissions::from_mode(0o0))
            .unwrap();
        local::publish_file(
            &download.parent,
            download.stage.as_ref().unwrap(),
            OsStr::new(PAYLOAD),
            download.writer().unwrap(),
            target.file_name().unwrap(),
        )
        .unwrap();
        // Model process exit after publication and before removing the empty stage.
        download.restore_private_access = false;
        download.keep_checkpoint();
        drop(download);
        assert!(root.join(&saved.stage_name).is_dir());
        assert!(StagedDownload::checkpoint_published(&saved, 14).unwrap());
        assert!(!root.join(&saved.stage_name).exists());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
            0
        );
        fs::set_permissions(target, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
