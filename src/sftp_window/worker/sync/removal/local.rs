use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io;
use std::path::{Component, Path};

use crate::sftp_window::files::LocalStamp;
use crate::sftp_window::local_tree::Directory;
use crate::sftp_window::tree::snapshot::{Entry as SnapshotEntry, LocalLeaf};
use crate::sftp_window::worker::resume::persist::Identity;
use crate::sftp_window::worker::{Failure, Result};

use super::Outcome;

mod replacement;
pub(in crate::sftp_window::worker::sync) use replacement::quarantine_for_replacement;

pub(super) fn remove(
    parent: &Path,
    name: &OsStr,
    expected_parent: &Identity,
    expected: &SnapshotEntry,
    before_write: &impl Fn() -> Result<()>,
) -> Result<Outcome> {
    require_child_name(name)?;
    let directory = match expected {
        SnapshotEntry::LocalFolder(_) => true,
        SnapshotEntry::LocalFile { .. } => false,
        SnapshotEntry::LocalLeaf(stamp) => {
            return platform::remove_leaf(parent, name, expected_parent, stamp, before_write);
        }
        SnapshotEntry::Remote(_) | SnapshotEntry::RemoteLink { .. } | SnapshotEntry::Blocked => {
            return Err(stale());
        }
    };
    platform::remove(
        parent,
        name,
        expected_parent,
        expected,
        directory,
        before_write,
    )
}

fn require_child_name(name: &OsStr) -> Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(part)) if part == name)
        || components.next().is_some()
    {
        return Err(stale());
    }
    Ok(())
}

fn check_parent(parent: &Directory, expected: &Identity) -> Result<()> {
    if Identity::directory(parent)? == *expected {
        Ok(())
    } else {
        Err(stale())
    }
}

fn check_entry(file: &File, expected: &SnapshotEntry) -> Result<Metadata> {
    let metadata = file.metadata()?;
    let identity = Identity::capture(file)?;
    let matches = match expected {
        SnapshotEntry::LocalFolder(saved) => metadata.is_dir() && *saved == identity,
        SnapshotEntry::LocalFile {
            stamp,
            identity: saved,
        } => metadata.is_file() && *saved == identity && *stamp == LocalStamp::capture(&metadata),
        SnapshotEntry::LocalLeaf(_) => false,
        SnapshotEntry::Remote(_) | SnapshotEntry::RemoteLink { .. } | SnapshotEntry::Blocked => {
            false
        }
    };
    if matches { Ok(metadata) } else { Err(stale()) }
}

fn stale() -> Failure {
    io::Error::other("A reviewed local entry or parent changed").into()
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub(super) fn remove_leaf(
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
        expected: &LocalLeaf,
        before_write: &impl Fn() -> Result<()>,
    ) -> Result<Outcome> {
        let ancestors = Directory::hold_reviewed_removal_ancestors(parent_path)?;
        let parent = ancestors.last().ok_or_else(stale)?;
        check_parent(parent, expected_parent)?;
        let file = parent.open_link_for_review(name, true)?;
        if LocalLeaf::capture(&file)?.as_ref() != Some(expected) {
            return Err(stale());
        }
        before_write()?;
        check_parent(&Directory::open_path(parent_path)?, expected_parent)?;
        if LocalLeaf::capture(&file)?.as_ref() != Some(expected) {
            return Err(stale());
        }
        kortty_files::tree::remove(file)?;
        confirm_removal(parent, parent_path, name, expected_parent)
    }

    pub(super) fn remove(
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
        expected: &SnapshotEntry,
        directory: bool,
        before_write: &impl Fn() -> Result<()>,
    ) -> Result<Outcome> {
        let parent = Directory::open_reviewed_removal_parent(parent_path)?;
        check_parent(&parent, expected_parent)?;
        let file = parent.child_for_reviewed_removal(name, directory)?;
        check_entry(&file, expected)?;
        before_write()?;
        check_parent(&Directory::open_path(parent_path)?, expected_parent)?;
        check_entry(&file, expected)?;
        match kortty_files::tree::remove(file) {
            Ok(()) => confirm_removal(&parent, parent_path, name, expected_parent),
            Err(error) if directory && error.kind() == io::ErrorKind::DirectoryNotEmpty => {
                Ok(Outcome::KeptNotEmpty)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn confirm_removal(
        parent: &Directory,
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
    ) -> Result<Outcome> {
        Directory::open_path(parent_path)
            .map_err(Failure::from)
            .and_then(|current| check_parent(&current, expected_parent))
            .and_then(|()| {
                parent
                    .require_absent_after_removal(name)
                    .map_err(Into::into)
            })
            .map_err(|error| error.context(" · Local removal could not be confirmed"))?;
        Ok(Outcome::Removed(None))
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::fs::MetadataExt as _;
    use std::path::PathBuf;

    use rustix::fs::{
        AtFlags, Dir, FileType, Mode, RenameFlags, Stat, fchmod, fstat, renameat_with, statat,
        unlinkat,
    };
    use rustix::io::Errno;
    use rustix::process::geteuid;

    use crate::sftp_window::{files::nonce, local_tree};

    use super::*;

    const PAYLOAD: &str = "entry";
    const NOFOLLOW: AtFlags = AtFlags::SYMLINK_NOFOLLOW;

    pub(super) fn remove(
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
        expected: &SnapshotEntry,
        directory: bool,
        before_write: &impl Fn() -> Result<()>,
    ) -> Result<Outcome> {
        let parent = Directory::open_path(parent_path)?;
        check_parent(&parent, expected_parent)?;
        check_protected(&parent)?;
        let entry = local_tree::entry(&parent, name)?;
        let file = match &entry {
            local_tree::Entry::File(file) if !directory => file,
            local_tree::Entry::Directory(folder) if directory => folder.file(),
            _ => return Err(stale()),
        };
        check_entry(file, expected)?;
        if directory && !empty(file)? {
            return Ok(Outcome::KeptNotEmpty);
        }
        let stage = Quarantine::create(parent_path, parent, nonce()?)?;
        let payload = OsStr::new(PAYLOAD);
        let move_into_stage = (|| -> Result<Option<(Stat, Metadata)>> {
            before_write()?;
            stage.verify_parent(expected_parent)?;
            stage.verify()?;
            let before = check_entry(file, expected)?;
            if directory && !empty(file)? {
                return Ok(None);
            }
            let moved = stage.relocate(name, payload, file)?;
            Ok(Some((moved, before)))
        })();
        let (moved, before) = match move_into_stage {
            Ok(Some(relocated)) => relocated,
            Ok(None) => {
                stage.cleanup()?;
                return Ok(Outcome::KeptNotEmpty);
            }
            Err(error) => return Err(stage.finish_failure(error)),
        };

        let remove_staged = (|| -> Result<bool> {
            let after_move = stage.check_payload(payload, file, &moved)?;
            if directory && !empty(file)? {
                return Ok(false);
            }
            if !unchanged_on_move(&before, &after_move) {
                return Err(stale());
            }
            before_write()?;
            stage.verify_parent(expected_parent)?;
            let current = stage.check_payload(payload, file, &moved)?;
            if directory && !empty(file)? {
                return Ok(false);
            }
            if LocalStamp::capture(&after_move) != LocalStamp::capture(&current)
                || after_move.nlink() != current.nlink()
            {
                return Err(stale());
            }
            let flags = if directory {
                AtFlags::REMOVEDIR
            } else {
                AtFlags::empty()
            };
            match unlinkat(stage.folder.file(), PAYLOAD, flags) {
                Ok(()) => Ok(true),
                Err(Errno::NOTEMPTY | Errno::EXIST) if directory => Ok(false),
                Err(error) => Err(io::Error::from(error).into()),
            }
        })();
        match remove_staged {
            Ok(true) => Ok(stage.finish_removal(name, expected_parent)),
            Ok(false) => {
                stage.restore(name, payload, &moved)?;
                stage.cleanup()?;
                Ok(Outcome::KeptNotEmpty)
            }
            Err(error) => match stage.restore(name, payload, &moved) {
                Ok(()) => Err(stage.finish_failure(error)),
                Err(restore) => Err(error.context(format!(" · {}", restore.message))),
            },
        }
    }

    pub(super) fn remove_leaf(
        parent_path: &Path,
        name: &OsStr,
        expected_parent: &Identity,
        expected: &LocalLeaf,
        before_write: &impl Fn() -> Result<()>,
    ) -> Result<Outcome> {
        let parent = Directory::open_path(parent_path)?;
        check_parent(&parent, expected_parent)?;
        check_protected(&parent)?;
        if LocalLeaf::from_stat(&statat(parent.file(), name, NOFOLLOW).map_err(io::Error::from)?)
            != *expected
        {
            return Err(stale());
        }
        let stage = Quarantine::create(parent_path, parent, nonce()?)?;
        let payload = OsStr::new(PAYLOAD);
        let moved = (|| -> Result<Stat> {
            before_write()?;
            stage.verify_parent(expected_parent)?;
            stage.verify()?;
            let named = statat(stage.parent.file(), name, NOFOLLOW).map_err(io::Error::from)?;
            if LocalLeaf::from_stat(&named) != *expected {
                return Err(stale());
            }
            stage.move_entry(name, payload)
        })()
        .map_err(|error| stage.finish_failure(error))?;
        let stamp = LocalLeaf::from_stat(&moved);
        let removed = (|| -> Result<()> {
            if !expected.same_after_rename(&stamp) {
                return Err(stale());
            }
            before_write()?;
            stage.verify_parent(expected_parent)?;
            stage.verify()?;
            let current =
                statat(stage.folder.file(), payload, NOFOLLOW).map_err(io::Error::from)?;
            if LocalLeaf::from_stat(&current) != stamp {
                return Err(stale());
            }
            unlinkat(stage.folder.file(), payload, AtFlags::empty()).map_err(io::Error::from)?;
            Ok(())
        })();
        match removed {
            Ok(()) => Ok(stage.finish_removal(name, expected_parent)),
            Err(error) => match stage.restore(name, payload, &moved) {
                Ok(()) => Err(stage.finish_failure(error)),
                Err(restore) => Err(error.context(format!(" · {}", restore.message))),
            },
        }
    }

    pub(super) struct Quarantine {
        parent_path: PathBuf,
        parent: Directory,
        name: String,
        folder: Directory,
    }

    impl Quarantine {
        pub(super) fn create(parent_path: &Path, parent: Directory, name: String) -> Result<Self> {
            let folder = parent.create_child(OsStr::new(&name)).map_err(|error| {
                Failure::from(error).context(quarantine_label(parent_path, &name))
            })?;
            let stage = Self {
                parent_path: parent_path.to_path_buf(),
                parent,
                name,
                folder,
            };
            let private = (|| -> Result<()> {
                stage.verify_name()?;
                #[cfg(target_os = "macos")]
                kortty_files::require_no_extended_acl(stage.folder.file())?;
                fchmod(stage.folder.file(), Mode::from_bits_truncate(0o700))
                    .map_err(io::Error::from)?;
                stage.verify()
            })();
            if let Err(error) = private {
                return Err(stage.finish_failure(error));
            }
            Ok(stage)
        }

        pub(super) fn path(&self, payload: &OsStr) -> PathBuf {
            self.parent_path.join(&self.name).join(payload)
        }

        pub(super) fn sync(&self) -> Result<()> {
            self.folder.file().sync_all()?;
            Ok(self.parent.file().sync_all()?)
        }

        pub(super) fn verify_parent(&self, expected: &Identity) -> Result<()> {
            let current = Directory::open_path(&self.parent_path)?;
            check_parent(&current, expected)?;
            if !self.parent.same_as(&current)? {
                return Err(stale());
            }
            check_protected(&current)
        }

        fn verify_name(&self) -> Result<()> {
            let named = statat(self.parent.file(), self.name.as_str(), NOFOLLOW)
                .map_err(io::Error::from)?;
            require_same(&named, &stat(self.folder.file())?)
        }

        pub(super) fn verify(&self) -> Result<()> {
            self.verify_name()?;
            let attributes = self.folder.file().metadata()?;
            if !attributes.is_dir()
                || attributes.uid() != geteuid().as_raw()
                || attributes.mode() & 0o7777 != 0o700
            {
                return Err(
                    io::Error::other("The local removal quarantine is no longer private").into(),
                );
            }
            #[cfg(target_os = "macos")]
            kortty_files::require_no_extended_acl(self.folder.file())?;
            Ok(())
        }

        pub(super) fn relocate(&self, name: &OsStr, payload: &OsStr, held: &File) -> Result<Stat> {
            let named = statat(self.parent.file(), name, NOFOLLOW).map_err(io::Error::from)?;
            require_same(&named, &stat(held)?)?;
            self.move_entry(name, payload)
        }

        fn move_entry(&self, name: &OsStr, payload: &OsStr) -> Result<Stat> {
            renameat_with(
                self.parent.file(),
                name,
                self.folder.file(),
                payload,
                RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)?;
            statat(self.folder.file(), payload, NOFOLLOW)
                .map_err(io::Error::from)
                .map_err(Into::into)
        }

        pub(super) fn check_payload(
            &self,
            payload: &OsStr,
            held: &File,
            moved: &Stat,
        ) -> Result<Metadata> {
            self.verify()?;
            require_same(moved, &stat(held)?)?;
            let named = statat(self.folder.file(), payload, NOFOLLOW).map_err(io::Error::from)?;
            require_same(&named, moved)?;
            Ok(held.metadata()?)
        }

        pub(super) fn restore(&self, name: &OsStr, payload: &OsStr, moved: &Stat) -> Result<()> {
            let result = (|| -> Result<()> {
                self.verify()?;
                let named =
                    statat(self.folder.file(), payload, NOFOLLOW).map_err(io::Error::from)?;
                require_same(&named, moved)?;
                renameat_with(
                    self.folder.file(),
                    payload,
                    self.parent.file(),
                    name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                let restored =
                    statat(self.parent.file(), name, NOFOLLOW).map_err(io::Error::from)?;
                require_same(&restored, moved)
            })();
            result.map_err(|error| {
                let entry = self.path(payload);
                error
                    .context(" · Could not verify restoration without replacement")
                    .context(format!(
                        " · Check local quarantine entry {}",
                        entry.as_os_str().as_encoded_bytes().escape_ascii()
                    ))
            })
        }

        pub(super) fn cleanup(&self) -> Result<()> {
            self.verify_name()
                .and_then(|()| {
                    unlinkat(self.parent.file(), self.name.as_str(), AtFlags::REMOVEDIR)
                        .map_err(io::Error::from)
                        .map_err(Into::into)
                })
                .map_err(|error| error.context(quarantine_label(&self.parent_path, &self.name)))
        }

        pub(super) fn discard_payload(
            &self,
            name: &OsStr,
            file: &File,
            moved: &Stat,
        ) -> Result<()> {
            self.check_payload(name, file, moved)?;
            unlinkat(self.folder.file(), name, AtFlags::empty())
                .map_err(io::Error::from)
                .map_err(Into::into)
        }

        pub(super) fn finish_failure(&self, error: Failure) -> Failure {
            match self.cleanup() {
                Ok(()) => error,
                Err(cleanup) => error.context(format!(" · {}", cleanup.message)),
            }
        }

        fn finish_removal(&self, name: &OsStr, expected_parent: &Identity) -> Outcome {
            let verified = self.verify_parent(expected_parent).and_then(|()| {
                match statat(self.parent.file(), name, NOFOLLOW) {
                    Err(Errno::NOENT) => Ok(()),
                    Ok(_) => {
                        Err(io::Error::other("The original local name is now occupied").into())
                    }
                    Err(error) => Err(io::Error::from(error).into()),
                }
            });
            let warning = match (verified, self.cleanup()) {
                (Err(error), Err(cleanup)) => {
                    Some(error.context(format!(" · {}", cleanup.message)))
                }
                (Err(error), Ok(())) | (Ok(()), Err(error)) => Some(error),
                (Ok(()), Ok(())) => None,
            };
            Outcome::Removed(warning)
        }
    }

    fn quarantine_label(parent: &Path, name: &str) -> String {
        let path = parent.join(name);
        format!(
            " · Check local quarantine {}",
            path.as_os_str().as_encoded_bytes().escape_ascii()
        )
    }

    pub(super) fn check_protected(parent: &Directory) -> Result<()> {
        let metadata = parent.file().metadata()?;
        if (metadata.uid() != 0 && metadata.uid() != geteuid().as_raw())
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::other(
                "Reviewed removal requires a protected local parent folder",
            )
            .into());
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(parent.file())?;
        Ok(())
    }

    pub(super) fn stat(file: &File) -> io::Result<Stat> {
        fstat(file).map_err(Into::into)
    }

    fn require_same(a: &Stat, b: &Stat) -> Result<()> {
        if a.st_dev == b.st_dev
            && a.st_ino == b.st_ino
            && FileType::from_raw_mode(a.st_mode) == FileType::from_raw_mode(b.st_mode)
        {
            Ok(())
        } else {
            Err(stale())
        }
    }

    pub(super) fn unchanged_on_move(before: &Metadata, after: &Metadata) -> bool {
        // The relocation itself can advance ctime, but must not change payload or permissions.
        before.mode() == after.mode()
            && before.uid() == after.uid()
            && before.gid() == after.gid()
            && before.nlink() == after.nlink()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
    }

    fn empty(file: &File) -> io::Result<bool> {
        for entry in Dir::read_from(file)? {
            if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::sftp_window::local_tree;
    use crate::snippets::tests::Profile;

    use super::*;

    fn reviewed(parent: &Path, name: &OsStr) -> (Identity, SnapshotEntry) {
        let parent = Directory::open_path(parent).unwrap();
        let expected = match local_tree::entry(&parent, name).unwrap() {
            local_tree::Entry::Directory(directory) => {
                SnapshotEntry::LocalFolder(Identity::directory(&directory).unwrap())
            }
            local_tree::Entry::File(file) => SnapshotEntry::LocalFile {
                stamp: LocalStamp::capture(&file.metadata().unwrap()),
                identity: Identity::capture(&file).unwrap(),
            },
            local_tree::Entry::Skip => panic!("expected a regular fixture entry"),
        };
        (Identity::directory(&parent).unwrap(), expected)
    }

    #[test]
    fn local_removal_uses_exact_identity_and_keeps_unselected_entries() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("selected");
        fs::write(root.join(name), b"same bytes").unwrap();
        fs::write(root.join("other"), b"same bytes").unwrap();
        let (parent, expected) = reviewed(&root, name);
        assert!(matches!(
            remove(&root, name, &parent, &expected, &|| Ok(())).unwrap(),
            Outcome::Removed(None)
        ));
        assert!(!root.join(name).exists());
        assert_eq!(fs::read(root.join("other")).unwrap(), b"same bytes");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn local_removal_refuses_a_replaced_entry_and_nonempty_folder() {
        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("selected");
        fs::write(root.join(name), b"same bytes").unwrap();
        let (parent, expected) = reviewed(&root, name);
        fs::rename(root.join(name), root.join("original")).unwrap();
        fs::write(root.join(name), b"same bytes").unwrap();
        assert!(remove(&root, name, &parent, &expected, &|| Ok(())).is_err());
        assert_eq!(fs::read(root.join(name)).unwrap(), b"same bytes");
        assert_eq!(fs::read(root.join("original")).unwrap(), b"same bytes");

        let folder = OsStr::new("folder");
        fs::create_dir(root.join(folder)).unwrap();
        let (parent, expected) = reviewed(&root, folder);
        fs::write(root.join(folder).join("unreviewed"), b"keep").unwrap();
        assert!(matches!(
            remove(&root, folder, &parent, &expected, &|| Ok(())).unwrap(),
            Outcome::KeptNotEmpty
        ));
        assert_eq!(
            fs::read(root.join(folder).join("unreviewed")).unwrap(),
            b"keep"
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn unix_local_leaf_removal_restores_the_staged_link_on_cancellation() {
        use std::cell::Cell;
        use std::os::unix::fs::symlink;

        use rustix::fs::{AtFlags, statat};

        use crate::sftp_window::tree::snapshot::LocalLeaf;

        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("selected-link");
        symlink("missing", root.join(name)).unwrap();
        let parent = Directory::open_path(&root).unwrap();
        let identity = Identity::directory(&parent).unwrap();
        let stamp = statat(parent.file(), name, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let calls = Cell::new(0);
        let result = remove(
            &root,
            name,
            &identity,
            &SnapshotEntry::LocalLeaf(LocalLeaf::from_stat(&stamp)),
            &|| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    assert!(fs::symlink_metadata(root.join(name)).is_err());
                    return Err(io::Error::other("Stopped by fixture").into());
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls.get(), 2);
        assert_eq!(
            fs::read_link(root.join(name)).unwrap(),
            Path::new("missing")
        );
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn unix_local_removal_keeps_quarantined_data_when_rollback_name_is_occupied() {
        use std::cell::Cell;

        let fixture = Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let name = OsStr::new("selected");
        fs::write(root.join(name), b"reviewed").unwrap();
        let (parent, expected) = reviewed(&root, name);
        let calls = Cell::new(0);
        let result = remove(&root, name, &parent, &expected, &|| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                fs::write(root.join(name), b"replacement")?;
                return Err(io::Error::other("Stopped by fixture").into());
            }
            Ok(())
        });
        let Err(error) = result else {
            panic!("expected the blocked restoration to be reported");
        };
        assert_eq!(calls.get(), 2);
        assert_eq!(fs::read(root.join(name)).unwrap(), b"replacement");
        let stage = fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .find(|entry| entry.file_name().to_string_lossy().starts_with(".kortty-"))
            .unwrap();
        assert_eq!(fs::read(stage.path().join("entry")).unwrap(), b"reviewed");
        assert!(error.message.contains("Check local quarantine entry"));
    }
}
