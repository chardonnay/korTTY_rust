//! Local review, recovery and publication for a generated file.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use pgp::crypto::hash::HashAlgorithm;
use zeroize::Zeroizing;

use crate::private_files::{FileStamp, require_single_link};
use crate::sftp_window::files::LocalStamp;
use crate::sftp_window::local_tree::Directory;
use crate::sftp_window::worker::resume::persist::Identity;

use super::*;

const MAX_ORIGINAL: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
pub(in crate::sftp_window) struct Origin {
    path: PathBuf,
    parent: Identity,
    stamp: LocalStamp,
    identity: Identity,
    digest: [u8; 32],
}

impl Origin {
    pub(in crate::sftp_window) fn capture(
        path: PathBuf,
        source: &LocalSource,
        bytes: &[u8],
    ) -> io::Result<Self> {
        let parent = source.parent_identity()?;
        let file = source.file();
        let stamp = LocalStamp::capture(&file.metadata()?);
        let identity = Identity::capture(file)?;
        source.verify_reviewed(&stamp, &identity, &parent)?;
        Ok(Self {
            path,
            parent,
            stamp,
            identity,
            digest: digest(bytes)?,
        })
    }

    pub(in crate::sftp_window) fn path(&self) -> &Path {
        &self.path
    }

    pub(in crate::sftp_window) fn has_digest(&self, digest: &[u8; 32]) -> bool {
        &self.digest == digest
    }
}

impl fmt::Debug for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Compared local draft destination")
    }
}

pub struct Plan {
    path: PathBuf,
    parent: Identity,
    target: SnapshotEntry,
    access: metadata::Metadata,
    original: Zeroizing<Vec<u8>>,
    original_digest: [u8; 32],
    original_bytes: usize,
    draft: Arc<str>,
    draft_digest: [u8; 32],
    backup: Option<PathBuf>,
    used: AtomicBool,
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reviewed local AI draft replacement")
    }
}

impl Plan {
    pub(in crate::sftp_window) fn prepare(
        path: PathBuf,
        draft: Arc<str>,
        backup: bool,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self> {
        Self::prepare_checked(path, draft, backup, None, stopped)
    }

    pub(in crate::sftp_window) fn prepare_compared(
        origin: &Origin,
        draft: Arc<str>,
        backup: bool,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self> {
        Self::prepare_checked(origin.path.clone(), draft, backup, Some(origin), stopped)
    }

    fn prepare_checked(
        path: PathBuf,
        draft: Arc<str>,
        backup: bool,
        origin: Option<&Origin>,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self> {
        require_running(stopped)?;
        if !valid_path(&path)
            || draft.is_empty()
            || draft.len() > crate::sftp_window::comparison_ai::MAX_DRAFT_BYTES
            || draft.contains('\0')
        {
            return Err(io::Error::other(
                "Choose an absolute existing local file and a valid draft",
            )
            .into());
        }
        let (directory, name) = path
            .parent()
            .zip(path.file_name())
            .expect("validated local path");
        let parent = Identity::directory(&Directory::open_path(directory)?)?;
        let original = LocalSource::open(&path)?;
        let file = original.file();
        require_single_link(file)?;
        let file_metadata = file.metadata()?;
        if file_metadata.permissions().readonly() || original.size() > MAX_ORIGINAL {
            return Err(
                io::Error::other("The local destination is read-only or exceeds 4 MiB").into(),
            );
        }
        let identity = Identity::capture(file)?;
        let stamp = LocalStamp::capture(&file_metadata);
        let access = metadata::Metadata::capture(file)?;
        #[cfg(unix)]
        require_no_extended_acl(file)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(original.size() + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != original.size() {
            return Err(io::Error::other("The local destination changed during review").into());
        }
        let original_bytes = bytes.len();
        let original_digest = digest(&bytes)?;
        if origin.is_some_and(|origin| {
            origin.parent != parent
                || origin.identity != identity
                || origin.stamp != stamp
                || origin.digest != original_digest
        }) {
            return Err(io::Error::other(
                "The compared right file changed. Reopen the comparison before replacing it",
            )
            .into());
        }
        let draft_digest = digest(draft.as_bytes())?;
        original.verify_reviewed(&stamp, &identity, &parent)?;
        require_single_link(file)?;
        access.require_matches(file)?;
        let backup = if backup {
            let mut backup = name.to_os_string();
            backup.push(".bak");
            if original.sibling_exists_or_uncertain(&backup) {
                return Err(io::Error::other(
                    "The .bak destination already exists or is unavailable",
                )
                .into());
            }
            Some(directory.join(backup))
        } else {
            None
        };
        require_running(stopped)?;
        Ok(Self {
            path,
            parent,
            target: SnapshotEntry::LocalFile { stamp, identity },
            access,
            original: if backup.is_some() {
                bytes
            } else {
                Zeroizing::new(Vec::new())
            },
            original_digest,
            original_bytes,
            draft,
            draft_digest,
            backup,
            used: AtomicBool::new(false),
        })
    }

    pub(in crate::sftp_window) fn matches(
        &self,
        path: &Path,
        draft: &Arc<str>,
        backup: bool,
    ) -> bool {
        self.path == path && Arc::ptr_eq(&self.draft, draft) && self.backup.is_some() == backup
    }

    pub(in crate::sftp_window) fn destination(&self) -> &Path {
        &self.path
    }

    pub(in crate::sftp_window) fn backup(&self) -> Option<&Path> {
        self.backup.as_deref()
    }

    pub(in crate::sftp_window) fn original_bytes(&self) -> usize {
        self.original_bytes
    }

    pub(in crate::sftp_window) fn draft_bytes(&self) -> usize {
        self.draft.len()
    }

    pub(in crate::sftp_window) fn draft_digest(&self) -> [u8; 32] {
        self.draft_digest
    }

    pub(in crate::sftp_window) fn digest_labels(&self) -> [String; 2] {
        [self.original_digest, self.draft_digest].map(digest_label)
    }

    pub(in crate::sftp_window) fn apply(
        &self,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> std::result::Result<(), String>,
    ) -> Result<String> {
        let permit = || -> Result<()> {
            require_running(stopped)?;
            authorize().map_err(io::Error::other)?;
            Ok(())
        };
        permit()?;
        if self.used.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("This local replacement review was already used").into());
        }
        let staged = StagedDownload::stage_for_replacement(&self.path, &self.parent)?;
        let mut file = staged.writer()?;
        file.write_all(self.draft.as_bytes())?;
        file.sync_all()?;
        let stamp = FileStamp::read(file)?;
        staged.verify_stage(&stamp)?;
        let optional_backup = self
            .backup
            .as_ref()
            .map(|path| -> Result<_> {
                let staged = StagedDownload::stage_new_download(path, &self.parent)?;
                let mut file = staged.writer()?;
                file.write_all(&self.original)?;
                file.sync_all()?;
                let stamp = FileStamp::read(file)?;
                staged.verify_stage(&stamp)?;
                Ok((staged, stamp))
            })
            .transpose()?;
        permit()?;
        let (parent, name) = self
            .path
            .parent()
            .zip(self.path.file_name())
            .expect("reviewed local path");
        let recovery = removal::local::quarantine_for_replacement(
            parent,
            name,
            &self.parent,
            (&self.target, self.original_digest),
            &|original| {
                permit()?;
                #[cfg(unix)]
                require_no_extended_acl(original)?;
                self.access.require_matches(original).map_err(Into::into)
            },
            stopped,
            None,
        )?;
        let apply_access = |file: &std::fs::File| -> io::Result<()> {
            permit().map_err(|error| io::Error::other(error.message))?;
            recovery
                .verify()
                .map_err(|error| io::Error::other(error.message))?;
            self.access.require_matches(recovery.file())?;
            #[cfg(unix)]
            require_same_unix_access_subjects(recovery.file(), file)?;
            self.access.apply_to(file)?;
            file.sync_all()?;
            self.access.require_matches(recovery.file())?;
            #[cfg(unix)]
            require_same_unix_access_subjects(recovery.file(), file)?;
            permit().map_err(|error| io::Error::other(error.message))
        };
        let mut saved_backup = false;
        let result = (|| -> Result<()> {
            if let Some((backup, stamp)) = optional_backup {
                backup.verify_partial(&stamp)?;
                if let Some(modified) = recovery
                    .file()
                    .metadata()
                    .ok()
                    .and_then(|data| data.modified().ok())
                {
                    backup.writer()?.set_modified(modified)?;
                }
                backup.publish_checked(apply_access)?;
                saved_backup = true;
            }
            staged.verify_partial(&stamp)?;
            staged.publish_checked(apply_access)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                let mut notice = format!("Replaced local file · {}", backup_label(&self.path));
                if let Some(path) = &self.backup {
                    notice.push_str(&format!(" · Backup {}", backup_label(path)));
                }
                if let Err(error) = recovery.discard() {
                    notice.push_str(&format!(" · {}", error.message));
                }
                Ok(notice)
            }
            Err(error) => {
                let path = backup_label(recovery.path());
                let mut error = match recovery.restore() {
                    Ok(()) => error.context(" · The original file was restored"),
                    Err(restore) => error.context(format!(
                        " · Could not restore the original ({}) · Recovery path {path}",
                        restore.message,
                    )),
                };
                if saved_backup && let Some(backup) = &self.backup {
                    error = error.context(format!(" · Backup kept at {}", backup_label(backup)));
                }
                Err(error)
            }
        }
    }
}

pub(in crate::sftp_window) fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().is_some()
        && path.as_os_str().as_encoded_bytes().len() <= 4096
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn digest(bytes: &[u8]) -> io::Result<[u8; 32]> {
    Ok(HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(io::Error::other)?
        .try_into()
        .expect("SHA-256 produces 32 bytes"))
}

#[cfg(unix)]
fn require_same_unix_access_subjects(
    original: &std::fs::File,
    staged: &std::fs::File,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let reviewed = original.metadata()?;
    let new = staged.metadata()?;
    if (reviewed.uid(), reviewed.gid()) != (new.uid(), new.gid()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The local replacement cannot preserve the original file owner or group",
        ));
    }
    require_no_extended_acl(original)?;
    require_no_extended_acl(staged)
}

#[cfg(target_os = "macos")]
use kortty_files::require_no_extended_acl;

#[cfg(target_os = "linux")]
fn require_no_extended_acl(file: &std::fs::File) -> io::Result<()> {
    use rustix::io::Errno;

    match rustix::fs::fgetxattr_query(file, "system.posix_acl_access") {
        Err(Errno::NODATA | Errno::NOTSUP) => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The local replacement cannot preserve an extended ACL",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::snippets::tests::Profile;

    use super::*;

    #[test]
    fn sftp_ai_local_draft_replaces_reviewed_bytes_with_optional_backup() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let destination = root.join("report.txt");
        let backup = root.join("report.txt.bak");
        fs::write(&destination, b"original").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let contents = Arc::<str>::from("draft");
        let plan =
            Plan::prepare(destination.clone(), Arc::clone(&contents), true, &|| false).unwrap();
        assert!(plan.matches(&destination, &contents, true));
        assert!(!plan.matches(&destination, &contents, false));
        assert!(!plan.matches(&destination, &Arc::from("draft"), true));
        assert_eq!(plan.backup(), Some(backup.as_path()));
        assert_eq!(
            plan.digest_labels(),
            [digest(b"original").unwrap(), digest(b"draft").unwrap()].map(digest_label)
        );
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert!(!backup.exists());
        assert!(
            plan.apply(&|| false, &|| Ok(()))
                .unwrap()
                .contains("Backup")
        );
        assert!(plan.apply(&|| false, &|| Ok(())).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"draft");
        assert_eq!(fs::read(&backup).unwrap(), b"original");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for path in [&destination, &backup] {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o640
                );
            }
        }

        let plan = Plan::prepare(
            destination.clone(),
            Arc::from("second draft"),
            false,
            &|| false,
        )
        .unwrap();
        assert!(plan.backup().is_none());
        plan.apply(&|| false, &|| Ok(())).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"second draft");
        assert_eq!(fs::read(&backup).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn sftp_ai_local_draft_rejects_changed_original_and_backup_collision() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let destination = root.join("report.txt");
        let backup = root.join("report.txt.bak");
        fs::write(&destination, b"original").unwrap();
        let review = || Plan::prepare(destination.clone(), Arc::from("draft"), true, &|| false);
        let plan = review().unwrap();
        fs::write(&destination, b"modified").unwrap();
        assert!(plan.apply(&|| false, &|| Ok(())).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"modified");
        assert!(!backup.exists());

        let plan = review().unwrap();
        fs::write(&backup, b"existing backup").unwrap();
        assert!(plan.apply(&|| false, &|| Ok(())).is_err());
        assert!(review().is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"modified");
        assert_eq!(fs::read(&backup).unwrap(), b"existing backup");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn sftp_ai_right_local_draft_origin_checks_the_compared_identity_and_raw_bytes() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let right = root.join("right.txt");
        let original = b"\xef\xbb\xbfright\r\n";
        fs::write(&right, original).unwrap();
        let capture = |bytes: &[u8]| {
            Origin::capture(right.clone(), &LocalSource::open(&right).unwrap(), bytes).unwrap()
        };
        let draft = Arc::<str>::from("draft\r\n");
        let prepare =
            |origin: &Origin| Plan::prepare_compared(origin, Arc::clone(&draft), true, &|| false);
        let origin = capture(original);
        let normalized = capture(b"right\n");
        assert!(prepare(&normalized).is_err());
        assert!(prepare(&origin).is_ok());
        fs::rename(&right, root.join("old.txt")).unwrap();
        fs::write(&right, original).unwrap();
        assert!(
            prepare(&origin)
                .unwrap_err()
                .message
                .contains("compared right file changed")
        );
        assert_eq!(fs::read(&right).unwrap(), original);
        assert!(!root.join("right.txt.bak").exists());
    }

    #[test]
    fn sftp_ai_local_draft_rolls_back_cancelled_publication_and_refuses_hardlinks() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let destination = root.join("report.txt");
        fs::write(&destination, b"original").unwrap();
        let review = || Plan::prepare(destination.clone(), Arc::from("draft"), false, &|| false);
        let plan = review().unwrap();
        let authorize = || {
            if destination.exists() {
                Ok(())
            } else {
                Err("Revoked after displacing the original".into())
            }
        };
        let failure = plan.apply(&|| false, &authorize).unwrap_err();
        assert!(failure.message.contains("The original file was restored"));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

        fs::hard_link(&destination, root.join("other.txt")).unwrap();
        assert!(review().is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sftp_ai_local_draft_rejects_existing_and_inherited_posix_access_acl() {
        use rustix::fs::{XattrFlags, fsetxattr};

        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let destination = root.join("report.txt");
        fs::write(&destination, b"original").unwrap();
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in [
            (1_u16, 6_u16, u32::MAX),
            (2, 4, 65534),
            (4, 0, u32::MAX),
            (16, 4, u32::MAX),
            (32, 0, u32::MAX),
        ] {
            acl.extend_from_slice(&tag.to_le_bytes());
            acl.extend_from_slice(&permissions.to_le_bytes());
            acl.extend_from_slice(&id.to_le_bytes());
        }
        let review = || Plan::prepare(destination.clone(), Arc::from("draft"), true, &|| false);
        let plan = review().unwrap();
        let file = fs::File::open(&destination).unwrap();
        fsetxattr(&file, "system.posix_acl_access", &acl, XattrFlags::empty()).unwrap();
        assert!(review().unwrap_err().message.contains("extended ACL"));
        assert!(plan.apply(&|| false, &|| Ok(())).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert!(!root.join("report.txt.bak").exists());
        rustix::fs::fremovexattr(&file, "system.posix_acl_access").unwrap();
        let plan = review().unwrap();
        let directory = fs::File::open(&root).unwrap();
        fsetxattr(
            &directory,
            "system.posix_acl_default",
            &acl,
            XattrFlags::empty(),
        )
        .unwrap();
        assert!(plan.apply(&|| false, &|| Ok(())).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert!(!root.join("report.txt.bak").exists());
    }
}
