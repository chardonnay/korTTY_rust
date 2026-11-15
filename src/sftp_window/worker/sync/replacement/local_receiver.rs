//! Checked local regular-file publication shared by the SFTP and rsync transports.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::fs::File;
use std::hash::Hasher;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};

use kortty_server_archive::rsync_tree::{Inventory, MAX_BYTES, Manifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::private_files::{FileStamp, require_single_link};
use crate::sftp_window::StagedDownload;
use crate::sftp_window::files::LocalStamp;
use crate::sftp_window::local_tree::{self, Directory, Entry, ReviewedStamp};
use crate::sftp_window::tree::single_file::FilePublishError;
use crate::sftp_window::tree::snapshot::Entry as SnapshotEntry;
use crate::sftp_window::worker::resume::persist::Identity;
use crate::sftp_window::worker::{Failure, Result};

use super::{backup_label, removal, require_running};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalFileSnapshot {
    pub(crate) inventory: Inventory,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) root_identity: [u8; 32],
}

impl LocalFileSnapshot {
    pub(crate) fn valid(&self) -> bool {
        single_file(self.inventory)
    }

    pub(crate) fn aliases(&self, other: &Self) -> bool {
        self.root_identity == other.root_identity
    }
}

pub(crate) enum ReviewedFilePublication {
    Published {
        successor: LocalFileSnapshot,
        recovery: PathBuf,
    },
    Restored {
        error: io::Error,
    },
    Uncertain {
        error: io::Error,
        recovery: Option<PathBuf>,
    },
}

pub(crate) struct ReviewedFileReceiver {
    path: PathBuf,
    parent: Directory,
    parent_identity: Identity,
    parent_stamp: ReviewedStamp,
    original: SnapshotEntry,
    witness: FileWitness,
}

impl ReviewedFileReceiver {
    pub(crate) fn inspect(
        path: &Path,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<LocalFileSnapshot> {
        Self::open(path, stopped).map(|receiver| receiver.snapshot(&receiver.witness))
    }

    pub(crate) fn prepare(
        path: &Path,
        expected: LocalFileSnapshot,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let receiver = Self::open(path, stopped)?;
        if expected.valid() && expected == receiver.snapshot(&receiver.witness) {
            Ok(receiver)
        } else {
            Err(changed())
        }
    }

    fn open(path: &Path, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        require_running(stopped)?;
        let (parent_path, name) = path.parent().zip(path.file_name()).ok_or_else(changed)?;
        reportable(
            &parent_path
                .join(".kortty-00000000000000000000000000000000.kortty-part")
                .join(name),
        )?;
        let parent = local_tree::open_reviewed_tree_parent(parent_path)?;
        let parent_stamp = local_tree::reviewed_tree_parent_stamp(&parent)?;
        let parent_identity = Identity::directory(&parent)?;
        let Entry::File(file) = local_tree::entry(&parent, name)? else {
            return Err(changed());
        };
        #[cfg(windows)]
        kortty_files::tree::require_reviewed_file_basename(&file, name)?;
        let witness = FileWitness::capture(&file, stopped)?;
        if witness.stamp.volume() != parent_stamp.volume() {
            return Err(changed());
        }
        let original = SnapshotEntry::LocalFile {
            stamp: LocalStamp::capture(&file.metadata()?),
            identity: Identity::capture(&file)?,
        };
        let receiver = Self {
            path: path.to_path_buf(),
            parent,
            parent_identity,
            parent_stamp,
            original,
            witness,
        };
        receiver.require_parent()?;
        receiver.require_named(&receiver.witness.stamp)?;
        require_running(stopped)?;
        Ok(receiver)
    }

    pub(crate) fn stage(&self) -> io::Result<StagedDownload> {
        self.require_parent()?;
        self.require_named(&self.witness.stamp)?;
        StagedDownload::stage_for_reviewed_file(&self.path, &self.parent_identity)
    }

    pub(crate) fn publish(
        self,
        stage: StagedDownload,
        incoming: Inventory,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> io::Result<()>,
        arm: impl FnOnce(u32) -> io::Result<()>,
    ) -> io::Result<ReviewedFilePublication> {
        require_running(stopped)?;
        authorize()?;
        let _ancestors = local_tree::hold_reviewed_file_ancestors(self.parent_path())?;
        self.require_parent()?;
        self.require_named(&self.witness.stamp)?;
        if stage.destination() != self.path || !single_file(incoming) {
            return Err(changed());
        }
        let input = FileWitness::capture(stage.writer()?, stopped)?;
        if input.inventory != incoming || input.stamp.volume() != self.parent_stamp.volume() {
            return Err(changed());
        }
        let stamp = FileStamp::read(stage.writer()?)?;
        stage.verify_stage(&stamp)?;
        let guard = || {
            require_running(stopped)?;
            self.require_parent()?;
            authorize()
        };
        let arm = RefCell::new(Some(arm));
        let before_move = |original: &File, staged: &File| -> Result<()> {
            #[cfg(unix)]
            rustix::fs::flock(
                original,
                rustix::fs::FlockOperation::NonBlockingLockExclusive,
            )
            .map_err(io::Error::from)?;
            if local_tree::reviewed_file_stamp(original)? != self.witness.stamp
                || local_tree::reviewed_file_stamp(staged)? != input.stamp
            {
                return Err(changed().into());
            }
            guard()?;
            arm.borrow_mut().take().ok_or_else(changed)?(0)?;
            guard()?;
            Ok(())
        };
        let file_before_publish = RefCell::new(None);
        let publish = |original: &File, file: &File| -> Result<()> {
            guard()?;
            if !self
                .witness
                .stamp
                .same_after_move(&local_tree::reviewed_file_stamp(original)?)
                || local_tree::reviewed_file_stamp(file)? != input.stamp
            {
                return Err(changed().into());
            }
            local_tree::copy_reviewed_file_metadata(original, file)?;
            file.sync_all()?;
            let final_file = FileWitness::capture(file, stopped)?;
            if final_file.inventory != incoming || !final_file.stamp.same_identity(&input.stamp) {
                return Err(changed().into());
            }
            file_before_publish.replace(Some(final_file));
            guard()?;
            Ok(())
        };
        let after = |parent: &Directory, name: &OsStr, file: &File| {
            local_tree::sync_reviewed_tree_directory(parent)?;
            let witness = FileWitness::capture(file, &|| false)?;
            let before = file_before_publish.borrow();
            let before = before.as_ref().ok_or_else(changed)?;
            if !before.stamp.same_after_move(&witness.stamp)
                || witness.inventory != incoming
                || !parent.same_as(&self.parent)?
                || name != self.name()
            {
                return Err(changed());
            }
            #[cfg(windows)]
            kortty_files::tree::require_reviewed_file_basename(file, name)?;
            self.require_parent()?;
            self.require_named(&witness.stamp)?;
            Ok(self.snapshot(&witness))
        };
        let result = commit_local(
            LocalTarget {
                folder: self.parent_path(),
                name: self.name(),
                parent: &self.parent_identity,
                original: (&self.original, self.witness.digest),
                backup_prefix: None,
            },
            stage,
            stopped,
            &before_move,
            &publish,
            after,
            true,
        );
        match result {
            Ok(LocalCommit::Published { receipt, recovery }) => {
                Ok(ReviewedFilePublication::Published {
                    successor: receipt,
                    recovery,
                })
            }
            Ok(LocalCommit::Restored(error)) => Ok(ReviewedFilePublication::Restored {
                error: failure_io(error),
            }),
            Ok(LocalCommit::Uncertain { error, recovery }) => {
                Ok(ReviewedFilePublication::Uncertain {
                    error: failure_io(error),
                    recovery,
                })
            }
            Err(error) => Err(failure_io(error)),
        }
    }

    fn name(&self) -> &OsStr {
        self.path.file_name().expect("checked receiver filename")
    }

    fn parent_path(&self) -> &Path {
        self.path.parent().expect("checked receiver parent")
    }

    fn require_parent(&self) -> io::Result<()> {
        let parent = local_tree::open_reviewed_tree_parent(self.parent_path())?;
        if self.parent.same_as(&parent)?
            && local_tree::reviewed_tree_parent_stamp(&parent)? == self.parent_stamp
        {
            Ok(())
        } else {
            Err(changed())
        }
    }

    fn require_named(&self, expected: &ReviewedStamp) -> io::Result<()> {
        let Entry::File(current) = local_tree::entry(&self.parent, self.name())? else {
            return Err(changed());
        };
        if local_tree::reviewed_file_stamp(&current)? == *expected {
            Ok(())
        } else {
            Err(changed())
        }
    }

    fn snapshot(&self, file: &FileWitness) -> LocalFileSnapshot {
        let mut identity = Fingerprint(Sha256::new_with_prefix(b"kortty-local-rsync-file-id-v1\0"));
        file.stamp.hash_identity_into(&mut identity);
        let mut proof = Fingerprint(Sha256::new_with_prefix(b"kortty-local-rsync-file-v1\0"));
        let path = self.path.as_os_str().as_encoded_bytes();
        proof.write_usize(path.len());
        proof.write(path);
        self.parent_stamp.hash_into(&mut proof);
        file.stamp.hash_into(&mut proof);
        proof.write(&file.inventory.digest);
        LocalFileSnapshot {
            inventory: file.inventory,
            fingerprint: proof.0.finalize().into(),
            root_identity: identity.0.finalize().into(),
        }
    }
}

struct FileWitness {
    stamp: ReviewedStamp,
    digest: [u8; 32],
    inventory: Inventory,
}

impl FileWitness {
    fn capture(mut file: &File, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        require_running(stopped)?;
        require_single_link(file)?;
        let stamp = local_tree::reviewed_file_stamp(file)?;
        let bytes = file.metadata()?.len();
        if bytes > MAX_BYTES {
            return Err(changed());
        }
        file.rewind()?;
        let mut hash = Sha256::new();
        let mut remaining = bytes;
        let mut buffer = [0; 64 * 1024];
        while remaining != 0 {
            require_running(stopped)?;
            let count = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..count])?;
            hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        if file.read(&mut buffer[..1])? != 0 || local_tree::reviewed_file_stamp(file)? != stamp {
            return Err(changed());
        }
        require_running(stopped)?;
        let digest = hash.finalize().into();
        let mut manifest = Manifest::default();
        manifest.add("", Some((bytes, digest)))?;
        Ok(Self {
            stamp,
            digest,
            inventory: manifest.finish(),
        })
    }
}

struct Fingerprint(Sha256);

impl Hasher for Fingerprint {
    fn finish(&self) -> u64 {
        let bytes = self.0.clone().finalize();
        u64::from_le_bytes(bytes[..8].try_into().expect("SHA-256 prefix"))
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
}

fn single_file(inventory: Inventory) -> bool {
    inventory.valid() && inventory.entries == 1 && inventory.files == 1
}

fn reportable(path: &Path) -> io::Result<()> {
    if path.is_absolute()
        && path.to_str().is_some_and(|text| {
            text.len() <= 4192
                && !crate::ssh_dialog::unsafe_text(text)
                && text
                    .chars()
                    .all(crate::session_journal::is_display_character)
        })
    {
        Ok(())
    } else {
        Err(changed())
    }
}

fn failure_io(error: Failure) -> io::Error {
    io::Error::other(error.message)
}

fn changed() -> io::Error {
    io::Error::other("The reviewed local file, identity, permissions, or volume changed")
}

pub(super) struct LocalTarget<'a> {
    pub(super) folder: &'a Path,
    pub(super) name: &'a OsStr,
    pub(super) parent: &'a Identity,
    pub(super) original: (&'a SnapshotEntry, [u8; 32]),
    pub(super) backup_prefix: Option<&'a str>,
}

pub(super) enum LocalCommit<T> {
    Published {
        receipt: T,
        recovery: PathBuf,
    },
    Restored(Failure),
    Uncertain {
        error: Failure,
        recovery: Option<PathBuf>,
    },
}

pub(super) fn commit_local<T>(
    target: LocalTarget<'_>,
    staged: StagedDownload,
    stopped: &impl Fn() -> bool,
    before_move: &impl Fn(&File, &File) -> Result<()>,
    finish_file: &impl Fn(&File, &File) -> Result<()>,
    after: impl FnOnce(&Directory, &OsStr, &File) -> io::Result<T>,
    durable: bool,
) -> Result<LocalCommit<T>> {
    let file_stamp = FileStamp::read(staged.writer()?)?;
    staged.verify_stage(&file_stamp)?;
    let might_have_moved = Cell::new(false);
    let backup = match removal::local::quarantine_for_replacement(
        target.folder,
        target.name,
        target.parent,
        target.original,
        &|file| {
            before_move(file, staged.writer()?)?;
            might_have_moved.set(true);
            Ok(())
        },
        stopped,
        target.backup_prefix,
    ) {
        Ok(backup) => backup,
        Err(error) if might_have_moved.get() => {
            return Ok(LocalCommit::Uncertain {
                error,
                recovery: None,
            });
        }
        Err(error) => return Err(error),
    };
    let publish = (|| {
        staged
            .verify_partial(&file_stamp)
            .map_err(FilePublishError::Before)?;
        backup
            .verify()
            .map_err(|error| FilePublishError::Before(failure_io(error)))?;
        if durable {
            backup
                .sync()
                .map_err(|error| FilePublishError::Before(failure_io(error)))?;
        }
        staged.publish_with_receipt(
            durable,
            |file| finish_file(backup.file(), file).map_err(failure_io),
            |parent, name, file| {
                if durable {
                    backup.sync().map_err(failure_io)?;
                }
                after(parent, name, file)
            },
        )
    })();
    match publish {
        Ok(receipt) => Ok(LocalCommit::Published {
            receipt,
            recovery: backup.retain(),
        }),
        Err(FilePublishError::Uncertain(error)) if durable => {
            let recovery = backup.verify().is_ok().then(|| backup.path().to_owned());
            Ok(LocalCommit::Uncertain {
                error: error.into(),
                recovery,
            })
        }
        Err(FilePublishError::Before(error) | FilePublishError::Uncertain(error)) => {
            let path = backup.path().to_owned();
            let recovery = backup.verify().is_ok().then(|| path.clone());
            let error = Failure::from(error);
            match backup.restore_with_sync(durable) {
                Ok(()) => Ok(LocalCommit::Restored(
                    error.context(" · The original file was restored"),
                )),
                Err(restore) => Ok(LocalCommit::Uncertain {
                    error: error.context(format!(
                        " · Restore or backup cleanup failed ({}) · Recovery path {}",
                        restore.message,
                        backup_label(&path)
                    )),
                    recovery,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;

    use crate::snippets::tests::Profile;

    use super::*;

    fn fixture(bytes: &[u8]) -> (Profile, PathBuf) {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = Directory::open_path(&root).unwrap();
        let workspace = OsStr::new("workspace");
        drop(local_tree::create_reviewed_tree_stage(&directory, workspace).unwrap());
        let destination = root.join(workspace).join("receiver.txt");
        write_fixture_file(&destination, bytes);
        (profile, destination)
    }

    fn write_fixture_file(path: &Path, bytes: &[u8]) {
        let parent = Directory::open_path(path.parent().unwrap()).unwrap();
        let mut file =
            local_tree::create_reviewed_tree_file(&parent, path.file_name().unwrap()).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn staged(receiver: &ReviewedFileReceiver, bytes: &[u8]) -> (StagedDownload, Inventory) {
        let stage = receiver.stage().unwrap();
        stage.writer().unwrap().write_all(bytes).unwrap();
        let incoming = FileWitness::capture(stage.writer().unwrap(), &|| false)
            .unwrap()
            .inventory;
        (stage, incoming)
    }

    #[test]
    fn checked_local_file_returns_current_successor_and_retains_original() {
        let (_fixture, destination) = fixture(b"old bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let old = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
        let receiver = ReviewedFileReceiver::prepare(&destination, old, &|| false).unwrap();
        let (stage, incoming) = staged(&receiver, b"new bytes");
        let publication = receiver
            .publish(stage, incoming, &|| false, &|| Ok(()), |removed| {
                assert_eq!(removed, 0);
                assert_eq!(fs::read(&destination).unwrap(), b"old bytes");
                Ok(())
            })
            .unwrap();
        let ReviewedFilePublication::Published {
            successor,
            recovery,
        } = publication
        else {
            panic!("the checked file must publish");
        };
        assert_eq!(
            successor,
            ReviewedFileReceiver::inspect(&destination, &|| false).unwrap()
        );
        assert_eq!(successor.inventory, incoming);
        assert!(!successor.aliases(&old));
        assert_eq!(fs::read(&destination).unwrap(), b"new bytes");
        assert_eq!(fs::read(&recovery).unwrap(), b"old bytes");
        assert_eq!(recovery.file_name(), destination.file_name());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                destination.metadata().unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
        fs::rename(
            &destination,
            destination.with_file_name("previous successor"),
        )
        .unwrap();
        write_fixture_file(&destination, b"new bytes");
        let other = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
        assert_eq!(other.inventory, successor.inventory);
        assert!(!other.aliases(&successor));
        assert!(ReviewedFileReceiver::prepare(&destination, successor, &|| false).is_err());
    }

    #[test]
    fn checked_local_file_restores_or_retains_when_publication_loses_the_name() {
        for occupant in [false, true] {
            let (_fixture, destination) = fixture(b"old");
            let old = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
            let receiver = ReviewedFileReceiver::prepare(&destination, old, &|| false).unwrap();
            let (stage, incoming) = staged(&receiver, b"new");
            let authorization = || {
                if !destination.exists() {
                    if occupant {
                        fs::write(&destination, b"intervening file")?;
                    }
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "policy changed",
                    ));
                }
                Ok(())
            };
            let publication = receiver
                .publish(stage, incoming, &|| false, &authorization, |_| Ok(()))
                .unwrap();
            if occupant {
                let ReviewedFilePublication::Uncertain {
                    recovery: Some(recovery),
                    ..
                } = publication
                else {
                    panic!("expected a retained recovery file");
                };
                assert_eq!(fs::read(&destination).unwrap(), b"intervening file");
                assert_eq!(fs::read(recovery).unwrap(), b"old");
            } else {
                assert!(matches!(
                    publication,
                    ReviewedFilePublication::Restored { .. }
                ));
                assert_eq!(fs::read(&destination).unwrap(), b"old");
                assert!(
                    old.aliases(&ReviewedFileReceiver::inspect(&destination, &|| false).unwrap())
                );
            }
        }
    }

    #[test]
    fn checked_local_file_rejects_shared_identity_and_unverified_content_before_arming() {
        let (_fixture, destination) = fixture(b"old");
        let link = destination.with_file_name("second name");
        fs::hard_link(&destination, &link).unwrap();
        assert!(ReviewedFileReceiver::inspect(&destination, &|| false).is_err());
        fs::remove_file(link).unwrap();
        let old = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
        let receiver = ReviewedFileReceiver::prepare(&destination, old, &|| false).unwrap();
        let (stage, _) = staged(&receiver, b"new");
        assert!(
            receiver
                .publish(
                    stage,
                    old.inventory,
                    &|| false,
                    &|| Ok(()),
                    |_| unreachable!()
                )
                .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"old");
    }
}
