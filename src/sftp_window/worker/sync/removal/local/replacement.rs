use std::ffi::OsString;
use std::io::{Read, Seek};
use std::path::PathBuf;

use pgp::crypto::hash::HashAlgorithm;

use crate::private_files::require_single_link;
use crate::sftp_window::worker::sync::require_running;

use super::*;

/// A verified original retained in a private sibling directory. Dropping this
/// handle only closes files; the displaced data remains recoverable.
pub(in crate::sftp_window::worker::sync) struct ReplacementBackup {
    path: PathBuf,
    name: OsString,
    expected_parent: Identity,
    file: File,
    stamp: LocalStamp,
    backup: native::Backup,
}

impl ReplacementBackup {
    pub(in crate::sftp_window::worker::sync) fn file(&self) -> &File {
        &self.file
    }

    pub(in crate::sftp_window::worker::sync) fn path(&self) -> &Path {
        &self.path
    }

    pub(in crate::sftp_window::worker::sync) fn verify(&self) -> Result<()> {
        let result = (|| {
            let metadata = self
                .backup
                .verify(&self.name, &self.file, &self.expected_parent)?;
            if self.stamp != LocalStamp::capture(&metadata) {
                return Err(stale());
            }
            require_single_link(&self.file)?;
            Ok(())
        })();
        result.map_err(|error| recovery_path(error, &self.path))
    }

    pub(in crate::sftp_window::worker::sync) fn restore(self) -> Result<()> {
        self.verify()?;
        let Self {
            path,
            name,
            expected_parent,
            file,
            backup,
            ..
        } = self;
        if let Err(error) = backup.restore(&name, &file, &expected_parent) {
            return Err(recovery_path(error, &path));
        }
        drop(file);
        backup
            .cleanup()
            .map_err(|error| recovery_path(error, &path))
    }

    pub(in crate::sftp_window::worker::sync) fn retain(self) -> PathBuf {
        self.path
    }

    pub(in crate::sftp_window::worker::sync) fn discard(self) -> Result<()> {
        self.verify()?;
        let Self {
            path,
            name,
            file,
            backup,
            ..
        } = self;
        backup
            .discard(&name, file)
            .map_err(|error| recovery_path(error, &path))?;
        backup
            .cleanup()
            .map_err(|error| recovery_path(error, &path))
    }
}

pub(in crate::sftp_window::worker::sync) fn quarantine_for_replacement(
    parent: &Path,
    name: &OsStr,
    expected_parent: &Identity,
    expected: (&SnapshotEntry, [u8; 32]),
    before_write: &impl Fn(&File) -> Result<()>,
    stopped: &impl Fn() -> bool,
    backup_prefix: Option<&str>,
) -> Result<ReplacementBackup> {
    require_child_name(name)?;
    if !matches!(expected.0, SnapshotEntry::LocalFile { .. }) {
        return Err(stale());
    }
    let (backup, file, stamp, path) = native::quarantine(
        parent,
        name,
        expected_parent,
        expected,
        before_write,
        stopped,
        backup_prefix,
    )?;
    Ok(ReplacementBackup {
        path,
        name: name.to_owned(),
        expected_parent: expected_parent.clone(),
        file,
        stamp,
        backup,
    })
}

fn require_content(
    file: &File,
    stamp: &LocalStamp,
    expected: [u8; 32],
    stopped: &impl Fn() -> bool,
) -> Result<()> {
    require_running(stopped)?;
    require_single_link(file)?;
    if LocalStamp::capture(&file.metadata()?) != *stamp {
        return Err(stale());
    }
    let mut reader = file;
    reader.rewind()?;
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = [0; 64 * 1024];
    let mut remaining = stamp.size;
    while remaining != 0 {
        require_running(stopped)?;
        let count = remaining.min(buffer.len() as u64) as usize;
        reader.read_exact(&mut buffer[..count])?;
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    require_running(stopped)?;
    if reader.read(&mut buffer[..1])? != 0
        || hash.finalize().as_ref() != expected.as_slice()
        || LocalStamp::capture(&file.metadata()?) != *stamp
    {
        return Err(io::Error::other("The reviewed local destination bytes changed").into());
    }
    require_single_link(file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::sftp_window::local_tree;
    use crate::snippets::tests::Profile;

    use super::*;

    fn reviewed(parent: &Path, name: &OsStr) -> (Identity, SnapshotEntry) {
        let parent = Directory::open_path(parent).unwrap();
        let local_tree::Entry::File(file) = local_tree::entry(&parent, name).unwrap() else {
            panic!("expected a regular fixture file");
        };
        (
            Identity::directory(&parent).unwrap(),
            SnapshotEntry::LocalFile {
                stamp: LocalStamp::capture(&file.metadata().unwrap()),
                identity: Identity::capture(&file).unwrap(),
            },
        )
    }

    fn digest(bytes: &[u8]) -> [u8; 32] {
        HashAlgorithm::Sha256
            .digest(bytes)
            .unwrap()
            .try_into()
            .unwrap()
    }

    #[test]
    fn replacement_backup_checks_identity_bytes_and_pre_move_cancellation() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        fs::write(root.join(name), b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        assert!(
            quarantine_for_replacement(
                &root,
                name,
                &parent,
                (&expected, digest(b"wrong")),
                &|_| Ok(()),
                &|| false,
                None,
            )
            .is_err()
        );
        assert!(
            quarantine_for_replacement(
                &root,
                name,
                &parent,
                (&expected, digest(b"original")),
                &|_| Err(io::Error::other("Test cancellation").into()),
                &|| false,
                None,
            )
            .is_err()
        );
        assert_eq!(fs::read(root.join(name)).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

        fs::rename(root.join(name), root.join("moved.txt")).unwrap();
        fs::write(root.join(name), b"original").unwrap();
        assert!(
            quarantine_for_replacement(
                &root,
                name,
                &parent,
                (&expected, digest(b"original")),
                &|_| Ok(()),
                &|| false,
                None,
            )
            .is_err()
        );
        assert_eq!(fs::read(root.join(name)).unwrap(), b"original");
        assert_eq!(fs::read(root.join("moved.txt")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn replacement_backup_retains_named_original_and_restores_only_to_absence() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        fs::write(root.join(name), b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let backup = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"original")),
            &|_| Ok(()),
            &|| false,
            None,
        )
        .unwrap();
        assert_eq!(backup.path().file_name(), Some(name));
        assert!(!root.join(name).exists());
        #[cfg(windows)]
        assert_eq!(
            fs::OpenOptions::new()
                .write(true)
                .open(backup.path())
                .unwrap_err()
                .raw_os_error(),
            Some(32),
        );
        backup.verify().unwrap();
        backup.restore().unwrap();
        assert_eq!(fs::read(root.join(name)).unwrap(), b"original");
        let (_, restored) = reviewed(&root, name);
        assert!(
            matches!((&expected, restored), (SnapshotEntry::LocalFile { identity: old, .. }, SnapshotEntry::LocalFile { identity: current, .. }) if old == &current)
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

        let (parent, expected) = reviewed(&root, name);
        let backup = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"original")),
            &|_| Ok(()),
            &|| false,
            None,
        )
        .unwrap();
        let retained = backup.path().to_path_buf();
        fs::write(root.join(name), b"new destination").unwrap();
        assert!(backup.restore().is_err());
        assert_eq!(fs::read(root.join(name)).unwrap(), b"new destination");
        assert_eq!(fs::read(&retained).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn replacement_backup_drop_and_retain_keep_displaced_bytes() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        fs::write(root.join(name), b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let backup = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"original")),
            &|_| Ok(()),
            &|| false,
            None,
        )
        .unwrap();
        let path = backup.path().to_path_buf();
        drop(backup);
        assert_eq!(fs::read(&path).unwrap(), b"original");

        fs::write(root.join(name), b"second original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let backup = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"second original")),
            &|_| Ok(()),
            &|| false,
            None,
        )
        .unwrap();
        let retained = backup.retain();
        assert_eq!(retained.file_name(), Some(name));
        assert_eq!(fs::read(&retained).unwrap(), b"second original");
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn replacement_backup_restores_original_when_post_move_proof_is_cancelled() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        fs::write(root.join(name), b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let result = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"original")),
            &|_| Ok(()),
            &|| !root.join(name).exists(),
            None,
        );
        let Err(error) = result else {
            panic!("expected post-move proof cancellation");
        };
        assert_eq!(fs::read(root.join(name)).unwrap(), b"original");
        let (_, restored) = reviewed(&root, name);
        assert!(
            matches!((&expected, restored), (SnapshotEntry::LocalFile { identity: old, .. }, SnapshotEntry::LocalFile { identity: current, .. }) if old == &current)
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert!(
            error
                .message
                .contains("The displaced local entry was restored")
        );
    }

    #[test]
    fn replacement_backup_does_not_overwrite_a_late_target_during_cancelled_proof() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        let target = root.join(name);
        fs::write(&target, b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let result = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"original")),
            &|_| Ok(()),
            &|| {
                if target.exists() {
                    return false;
                }
                fs::write(&target, b"late target").unwrap();
                true
            },
            None,
        );
        let Err(error) = result else {
            panic!("expected post-move proof cancellation");
        };
        assert_eq!(fs::read(&target).unwrap(), b"late target");
        let stage = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| entry.is_dir())
            .unwrap();
        assert_eq!(fs::read(stage.join(name)).unwrap(), b"original");
        assert!(error.message.contains("Check the retained local entry"));
    }

    #[cfg(windows)]
    #[test]
    fn scheduled_replacement_groups_recovery_names_and_keeps_the_original() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        let target = root.join(name);
        fs::write(&target, b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let digest = digest(b"original");

        let invalid = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest),
            &|_| Ok(()),
            &|| false,
            Some("../escape"),
        );
        assert!(invalid.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

        let prefix = "0123456789abcdef";
        let backup = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest),
            &|held| {
                assert!(fs::OpenOptions::new().write(true).open(&target).is_err());
                let SnapshotEntry::LocalFile { identity, .. } = &expected else {
                    unreachable!();
                };
                assert!(Identity::capture(held)? == *identity);
                Ok(())
            },
            &|| false,
            Some(prefix),
        )
        .unwrap();
        let retained = backup.retain();
        let folder = retained
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(folder.len(), ".kortty-".len() + 32 + ".kortty-part".len());
        assert!(folder.starts_with(&format!(".kortty-{prefix}")));
        assert!(folder.ends_with(".kortty-part"));
        assert!(!target.exists());
        assert_eq!(fs::read(retained).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn replacement_backup_refuses_to_restore_same_inode_with_changed_metadata() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("original.txt");
        fs::write(root.join(name), b"original").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let backup = quarantine_for_replacement(
            &root,
            name,
            &parent,
            (&expected, digest(b"original")),
            &|_| Ok(()),
            &|| false,
            None,
        )
        .unwrap();
        let retained = backup.path().to_path_buf();
        fs::write(&retained, b"changed").unwrap();
        assert!(backup.restore().is_err());
        assert!(!root.join(name).exists());
        assert_eq!(fs::read(&retained).unwrap(), b"changed");
    }
}

fn recovery_path(error: Failure, path: &Path) -> Failure {
    error.context(format!(
        " · Check the retained local entry {}",
        path.as_os_str().as_encoded_bytes().escape_ascii()
    ))
}

fn recover_error(error: Failure, recovery: Result<()>, path: &Path) -> Failure {
    match recovery {
        Ok(()) => error,
        Err(recovery) => recovery_path(error.context(format!(" · {}", recovery.message)), path),
    }
}

fn rollback_failed_proof(
    error: Failure,
    restore: Result<()>,
    cleanup: impl FnOnce() -> Result<()>,
    path: &Path,
) -> Failure {
    match restore {
        Ok(()) => recover_error(
            error.context(" · The displaced local entry was restored"),
            cleanup(),
            path,
        ),
        Err(restore) => recover_error(error, Err(restore), path),
    }
}

#[cfg(unix)]
mod native {
    use crate::sftp_window::local_tree;

    use super::super::platform::{Quarantine, check_protected, stat, unchanged_on_move};
    use super::*;

    pub(super) struct Backup(Quarantine);

    impl Backup {
        pub(super) fn verify(
            &self,
            name: &OsStr,
            file: &File,
            parent: &Identity,
        ) -> Result<Metadata> {
            self.0.verify_parent(parent)?;
            self.0.check_payload(name, file, &stat(file)?)
        }

        pub(super) fn restore(&self, name: &OsStr, file: &File, parent: &Identity) -> Result<()> {
            self.0.verify_parent(parent)?;
            self.0.restore(name, name, &stat(file)?)
        }

        pub(super) fn cleanup(self) -> Result<()> {
            self.0.cleanup()
        }

        pub(super) fn discard(&self, name: &OsStr, file: File) -> Result<()> {
            self.0.discard_payload(name, &file, &stat(&file)?)
        }
    }

    pub(super) fn quarantine(
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
        (expected, expected_digest): (&SnapshotEntry, [u8; 32]),
        before_write: &impl Fn(&File) -> Result<()>,
        stopped: &impl Fn() -> bool,
        _backup_prefix: Option<&str>,
    ) -> Result<(Backup, File, LocalStamp, PathBuf)> {
        let parent = Directory::open_path(parent_path)?;
        check_parent(&parent, expected_parent)?;
        check_protected(&parent)?;
        let local_tree::Entry::File(file) = local_tree::entry(&parent, name)? else {
            return Err(stale());
        };
        let before = check_entry(&file, expected)?;
        require_content(
            &file,
            &LocalStamp::capture(&before),
            expected_digest,
            stopped,
        )?;
        let stage = Quarantine::create(parent_path, parent)?;
        let path = stage.path(name);
        let moved = (|| {
            before_write(&file)?;
            stage.verify_parent(expected_parent)?;
            stage.verify()?;
            check_entry(&file, expected)?;
            stage.relocate(name, name, &file)
        })();
        let moved = match moved {
            Ok(moved) => moved,
            Err(error) => return Err(recover_error(error, stage.cleanup(), &path)),
        };
        let proof = (|| {
            let after = stage.check_payload(name, &file, &moved)?;
            if !unchanged_on_move(&before, &after) {
                return Err(stale());
            }
            let stamp = LocalStamp::capture(&after);
            require_content(&file, &stamp, expected_digest, stopped)?;
            stage.check_payload(name, &file, &moved)?;
            stage.verify_parent(expected_parent)?;
            Ok(stamp)
        })();
        match proof {
            Ok(stamp) => Ok((Backup(stage), file, stamp, path)),
            Err(error) => {
                // Roll back only the held original. A different entry moved by
                // a racing rename must stay available at the recovery path.
                let restore = stage
                    .verify_parent(expected_parent)
                    .and_then(|()| stage.check_payload(name, &file, &moved))
                    .and_then(|_| stage.restore(name, name, &moved))
                    .and_then(|()| stage.verify_parent(expected_parent));
                Err(rollback_failed_proof(
                    error,
                    restore,
                    || stage.cleanup(),
                    &path,
                ))
            }
        }
    }
}

#[cfg(windows)]
mod native {
    use crate::sftp_window::files::nonce;

    use super::*;

    pub(super) struct Backup {
        parent_path: PathBuf,
        parent: Directory,
        stage_name: OsString,
        stage: Directory,
    }

    impl Backup {
        fn check_stage(&self) -> Result<()> {
            self.stage.require_owner_only()?;
            let current = self
                .parent
                .read_child(&self.stage_name)?
                .ok_or_else(stale)?;
            let current = Directory::from_file(current)?;
            if !self.stage.same_as(&current)? {
                return Err(stale());
            }
            Ok(())
        }

        fn check_named(folder: &Directory, name: &OsStr, file: &File) -> Result<Metadata> {
            let current = folder.read_child(name)?.ok_or_else(stale)?;
            if Identity::capture(&current)? != Identity::capture(file)? {
                return Err(stale());
            }
            Ok(file.metadata()?)
        }

        pub(super) fn verify(
            &self,
            name: &OsStr,
            file: &File,
            parent: &Identity,
        ) -> Result<Metadata> {
            check_parent(&Directory::open_path(&self.parent_path)?, parent)?;
            self.check_stage()?;
            Self::check_named(&self.stage, name, file)
        }

        pub(super) fn restore(&self, name: &OsStr, file: &File, parent: &Identity) -> Result<()> {
            self.verify(name, file, parent)?;
            self.parent.publish_file(file, name)?;
            Self::check_named(&self.parent, name, file)?;
            check_parent(&Directory::open_path(&self.parent_path)?, parent)
        }

        pub(super) fn cleanup(self) -> Result<()> {
            self.check_stage()?;
            Ok(self.stage.remove()?)
        }

        pub(super) fn discard(&self, name: &OsStr, file: File) -> Result<()> {
            self.check_stage()?;
            Self::check_named(&self.stage, name, &file)?;
            Ok(kortty_files::tree::remove(file)?)
        }
    }

    pub(super) fn quarantine(
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
        (expected, expected_digest): (&SnapshotEntry, [u8; 32]),
        before_write: &impl Fn(&File) -> Result<()>,
        stopped: &impl Fn() -> bool,
        backup_prefix: Option<&str>,
    ) -> Result<(Backup, File, LocalStamp, PathBuf)> {
        let parent = Directory::open_reviewed_removal_parent(parent_path)?;
        check_parent(&parent, expected_parent)?;
        let file = parent.child_for_reviewed_removal(name, false)?;
        let stamp = LocalStamp::capture(&check_entry(&file, expected)?);
        require_content(&file, &stamp, expected_digest, stopped)?;
        let mut stage_name = nonce()?;
        if let Some(prefix) = backup_prefix {
            if prefix.len() != 16
                || !prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(io::Error::other("Invalid scheduled replacement backup prefix").into());
            }
            let start = ".kortty-".len();
            stage_name.replace_range(start..start + prefix.len(), prefix);
        }
        let stage_name = OsString::from(stage_name);
        let stage = parent.create_backup_directory(&stage_name)?;
        let path = parent_path.join(&stage_name).join(name);
        let backup = Backup {
            parent_path: parent_path.to_path_buf(),
            parent,
            stage_name,
            stage,
        };
        let moved = (|| {
            before_write(&file)?;
            check_parent(&Directory::open_path(parent_path)?, expected_parent)?;
            backup.check_stage()?;
            check_entry(&file, expected)?;
            Backup::check_named(&backup.parent, name, &file)?;
            Ok(backup.stage.publish_file(&file, name)?)
        })();
        if let Err(error) = moved {
            return Err(recover_error(error, backup.cleanup(), &path));
        }
        let proof = (|| {
            backup.verify(name, &file, expected_parent)?;
            require_content(&file, &stamp, expected_digest, stopped)?;
            backup.verify(name, &file, expected_parent)?;
            Ok(())
        })();
        match proof {
            Ok(()) => Ok((backup, file, stamp, path)),
            Err(error) => {
                let restore = backup.restore(name, &file, expected_parent);
                Err(rollback_failed_proof(
                    error,
                    restore,
                    || {
                        drop(file);
                        backup.cleanup()
                    },
                    &path,
                ))
            }
        }
    }
}
