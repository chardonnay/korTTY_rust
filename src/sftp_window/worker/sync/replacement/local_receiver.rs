//! Checked local regular-file publication shared by the SFTP and rsync transports.

use std::cell::{Cell, RefCell};
use std::ffi::OsStr;
use std::fs::File;
use std::hash::Hasher;
use std::io::{self, Read as _, Seek as _};
use std::path::{Path, PathBuf};

use kortty_files::DirectoryNameRules;
use kortty_server_archive::rsync_tree::{Inventory, MAX_BYTES, Manifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::job_executor::rsync::LocalTreeLocation;
use crate::private_files::{FileStamp, require_single_link};
use crate::sftp_window::StagedDownload;
use crate::sftp_window::files::LocalStamp;
use crate::sftp_window::local_tree::{self, Directory, Entry, ReviewedStamp};
use crate::sftp_window::privileged_file::MAX_TRANSFER_BYTES;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) alias: Option<VacantLocalFileAlias>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) location: Option<LocalTreeLocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) published_from: Option<[u8; 32]>,
}

impl LocalFileSnapshot {
    pub(crate) fn valid(&self) -> bool {
        single_file(self.inventory, MAX_BYTES)
            && valid_location(self.alias(), self.location())
            && self
                .published_from
                .is_none_or(|proof| proof != [0; 32] && self.location.is_some())
    }

    pub(crate) fn aliases(&self, other: &Self) -> bool {
        self.root_identity == other.root_identity
    }

    pub(crate) fn alias(&self) -> Option<&VacantLocalFileAlias> {
        self.alias.as_ref()
    }

    pub(crate) fn location(&self) -> Option<&LocalTreeLocation> {
        self.location.as_ref()
    }
}

pub(crate) enum ReviewedFilePublication {
    Published {
        successor: Box<LocalFileSnapshot>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VacantLocalFileSnapshot {
    fingerprint: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<VacantLocalFileAlias>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<LocalTreeLocation>,
}

impl VacantLocalFileSnapshot {
    pub(crate) fn valid(&self) -> bool {
        self.fingerprint != [0; 32] && valid_location(self.alias(), self.location())
    }

    pub(crate) fn alias(&self) -> Option<&VacantLocalFileAlias> {
        self.alias.as_ref()
    }

    pub(crate) fn location(&self) -> Option<&LocalTreeLocation> {
        self.location.as_ref()
    }
}

fn valid_location(
    alias: Option<&VacantLocalFileAlias>,
    location: Option<&LocalTreeLocation>,
) -> bool {
    alias.is_none_or(VacantLocalFileAlias::valid)
        && location.is_none_or(|location| location.valid() && alias == Some(location.alias()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VacantLocalFileAlias {
    parent: [u8; 32],
    name: AliasName,
}

impl VacantLocalFileAlias {
    pub(crate) fn capture(directory: &Directory, parent: &ReviewedStamp, name: &OsStr) -> Self {
        let mut identity = Fingerprint(Sha256::new_with_prefix(
            b"kortty-local-rsync-file-parent-id-v1\0",
        ));
        parent.hash_identity_into(&mut identity);
        Self {
            parent: identity.0.finalize().into(),
            name: AliasName::capture(directory, name),
        }
    }

    pub(crate) fn valid(&self) -> bool {
        self.parent != [0; 32]
    }

    pub(crate) fn aliases(&self, other: &Self) -> bool {
        self.parent == other.parent && self.name.aliases(other.name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AliasName {
    Literal([u8; 32]),
    Normalized([u8; 32]),
    FoldedAscii([u8; 32]),
    Indeterminate,
}

impl AliasName {
    fn capture(directory: &Directory, name: &OsStr) -> Self {
        let bytes = name.as_encoded_bytes();
        #[cfg(windows)]
        let rules = directory.name_lookup_rules();
        #[cfg(unix)]
        let rules = kortty_files::directory_name_rules(directory.file());
        // A Windows short spelling can resolve to a different long name in the same directory.
        if cfg!(windows) && bytes.contains(&b'~') {
            return Self::Indeterminate;
        }
        match rules {
            Some(DirectoryNameRules::Literal) => Self::Literal(Sha256::digest(bytes).into()),
            Some(DirectoryNameRules::Normalized) => {
                use unicode_normalization::UnicodeNormalization as _;

                let Some(name) = name.to_str() else {
                    return Self::Indeterminate;
                };
                let normalized: String = name.nfd().collect();
                Self::Normalized(Sha256::digest(normalized.as_bytes()).into())
            }
            Some(DirectoryNameRules::Folded) if bytes.is_ascii() => {
                Self::FoldedAscii(Sha256::digest(bytes.to_ascii_uppercase()).into())
            }
            _ => Self::Indeterminate,
        }
    }

    fn aliases(self, other: Self) -> bool {
        match (self, other) {
            (Self::Literal(left), Self::Literal(right))
            | (Self::Normalized(left), Self::Normalized(right))
            | (Self::FoldedAscii(left), Self::FoldedAscii(right)) => left == right,
            // The filesystem's normalization or case table could equate the names.
            _ => true,
        }
    }
}

pub(crate) enum VacantLocalFilePublication {
    Published(Box<LocalFileSnapshot>),
    Uncertain(io::Error),
}

pub(crate) struct VacantLocalFileReceiver {
    path: PathBuf,
    parent: Directory,
    parent_identity: Identity,
    parent_stamp: ReviewedStamp,
}

impl VacantLocalFileReceiver {
    pub(crate) fn inspect(
        path: &Path,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<VacantLocalFileSnapshot> {
        let receiver = Self::open(path, stopped)?;
        let snapshot = receiver.snapshot();
        if snapshot.valid() {
            Ok(snapshot)
        } else {
            Err(changed())
        }
    }

    pub(crate) fn prepare(
        path: &Path,
        expected: VacantLocalFileSnapshot,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let receiver = Self::open(path, stopped)?;
        let mut current = receiver.snapshot();
        if expected.alias.is_none() {
            current.alias = None;
        }
        if expected.location.is_none() {
            current.location = None;
        }
        if expected.valid() && expected == current {
            Ok(receiver)
        } else {
            Err(changed())
        }
    }

    pub(crate) fn open(path: &Path, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        require_running(stopped)?;
        let (parent_path, name) = checked_location(path)?;
        crate::sftp_window::files::local_name(name.to_str().ok_or_else(changed)?.as_bytes())?;
        if !path
            .as_os_str()
            .as_encoded_bytes()
            .ends_with(name.as_encoded_bytes())
        {
            return Err(changed());
        }
        let parent = local_tree::open_reviewed_tree_parent(parent_path)?;
        let receiver = Self {
            path: path.to_path_buf(),
            parent_stamp: local_tree::reviewed_tree_parent_stamp(&parent)?,
            parent_identity: Identity::directory(&parent)?,
            parent,
        };
        receiver.require_vacant()?;
        require_running(stopped)?;
        Ok(receiver)
    }

    pub(crate) fn stage(&self) -> io::Result<StagedDownload> {
        self.require_vacant()?;
        let stage = StagedDownload::stage_for_reviewed_file(&self.path, &self.parent_identity)?;
        self.require_vacant()?;
        Ok(stage)
    }

    pub(crate) fn publish(
        self,
        stage: StagedDownload,
        incoming: Inventory,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> io::Result<()>,
        arm: impl FnOnce(u32) -> io::Result<()>,
    ) -> io::Result<VacantLocalFilePublication> {
        require_running(stopped)?;
        authorize()?;
        let _ancestors = local_tree::hold_reviewed_file_ancestors(self.parent_path())?;
        self.require_vacant()?;
        if stage.destination().as_os_str() != self.path.as_os_str()
            || !single_file(incoming, MAX_BYTES)
        {
            return Err(changed());
        }
        stage.verify_reviewed_parent(&self.parent_identity)?;
        let input = FileWitness::capture(stage.writer()?, stopped)?;
        if input.inventory != incoming || input.stamp.volume() != self.parent_stamp.volume() {
            return Err(changed());
        }
        let stamp = FileStamp::read(stage.writer()?)?;
        stage.verify_partial(&stamp)?;
        let guard = |file: &File| {
            require_running(stopped)?;
            authorize()?;
            self.require_vacant()?;
            require_single_link(file)?;
            if local_tree::reviewed_file_stamp(file)? == input.stamp {
                Ok(())
            } else {
                Err(changed())
            }
        };
        let publish = stage.publish_with_receipt(
            true,
            |file| {
                guard(file)?;
                arm(0)?;
                guard(file)
            },
            |parent, name, file| {
                local_tree::sync_reviewed_tree_directory(parent)?;
                let witness = FileWitness::capture(file, &|| false)?;
                if !input.stamp.same_after_first_publication(&witness.stamp)
                    || witness.inventory != incoming
                    || !parent.same_as(&self.parent)?
                    || name != self.name()
                {
                    return Err(changed());
                }
                #[cfg(windows)]
                kortty_files::tree::require_reviewed_file_basename(file, name)?;
                require_bound_parent(self.parent_path(), &self.parent, &self.parent_stamp)?;
                require_named_file(&self.parent, self.name(), &witness.stamp)?;
                Ok(file_snapshot(
                    &self.path,
                    &self.parent,
                    &self.parent_stamp,
                    &witness,
                ))
            },
        );
        match publish {
            Ok(snapshot) => Ok(VacantLocalFilePublication::Published(Box::new(snapshot))),
            Err(FilePublishError::Before(error)) => Err(error),
            Err(FilePublishError::Uncertain(error)) => {
                Ok(VacantLocalFilePublication::Uncertain(error))
            }
        }
    }

    fn name(&self) -> &OsStr {
        self.path.file_name().expect("checked receiver filename")
    }

    fn parent_path(&self) -> &Path {
        self.path.parent().expect("checked receiver parent")
    }

    fn snapshot(&self) -> VacantLocalFileSnapshot {
        let mut proof = path_fingerprint(b"kortty-vacant-local-rsync-file-v1\0", &self.path);
        self.parent_stamp.hash_into(&mut proof);
        let alias = VacantLocalFileAlias::capture(&self.parent, &self.parent_stamp, self.name());
        VacantLocalFileSnapshot {
            fingerprint: proof.0.finalize().into(),
            alias: Some(alias),
            location: Some(LocalTreeLocation::new(
                alias,
                self.parent_stamp.volume().mount_identity(),
            )),
        }
    }

    fn require_vacant(&self) -> io::Result<()> {
        require_bound_parent(self.parent_path(), &self.parent, &self.parent_stamp)?;
        #[cfg(windows)]
        return self.parent.require_absent_after_removal(self.name());
        #[cfg(unix)]
        match rustix::fs::statat(
            self.parent.file(),
            self.name(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => Ok(()),
            Ok(_) => Err(changed()),
            Err(error) => Err(error.into()),
        }
    }
}

pub(crate) struct ReviewedFileReceiver {
    path: PathBuf,
    parent: Directory,
    parent_identity: Identity,
    parent_stamp: ReviewedStamp,
    original: SnapshotEntry,
    witness: FileWitness,
    max_bytes: u64,
}

impl ReviewedFileReceiver {
    pub(crate) fn inspect(
        path: &Path,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<LocalFileSnapshot> {
        Self::open(path, stopped).map(|receiver| receiver.snapshot(&receiver.witness))
    }

    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        self.snapshot(&self.witness).fingerprint
    }

    pub(in crate::sftp_window) fn content(&self) -> (u64, [u8; 32]) {
        (self.witness.inventory.bytes, self.witness.digest)
    }

    pub(crate) fn inspect_transfer(
        path: &Path,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<LocalFileSnapshot> {
        Self::open_transfer(path, stopped).map(|receiver| receiver.snapshot(&receiver.witness))
    }

    pub(crate) fn prepare(
        path: &Path,
        mut expected: LocalFileSnapshot,
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let receiver = Self::open(path, stopped)?;
        let mut current = receiver.snapshot(&receiver.witness);
        if expected.alias.is_none() {
            current.alias = None;
        }
        if expected.location.is_none() {
            current.location = None;
        }
        let valid = expected.valid();
        expected.published_from = None;
        if valid && expected == current {
            Ok(receiver)
        } else {
            Err(changed())
        }
    }

    pub(crate) fn open(path: &Path, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        Self::open_bounded(path, stopped, MAX_BYTES)
    }

    pub(crate) fn open_transfer(path: &Path, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        Self::open_bounded(path, stopped, MAX_TRANSFER_BYTES)
    }

    fn open_bounded(path: &Path, stopped: &impl Fn() -> bool, max_bytes: u64) -> io::Result<Self> {
        require_running(stopped)?;
        let (parent_path, name) = checked_location(path)?;
        let parent = local_tree::open_reviewed_tree_parent(parent_path)?;
        let parent_stamp = local_tree::reviewed_tree_parent_stamp(&parent)?;
        let parent_identity = Identity::directory(&parent)?;
        let Entry::File(file) = local_tree::entry(&parent, name)? else {
            return Err(changed());
        };
        #[cfg(windows)]
        kortty_files::tree::require_reviewed_file_basename(&file, name)?;
        let witness = FileWitness::capture_bounded(&file, stopped, max_bytes)?;
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
            max_bytes,
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
        self.publish_with_recovery_prefix(stage, incoming, stopped, authorize, arm, None)
    }

    pub(in crate::sftp_window) fn publish_with_recovery_prefix(
        self,
        stage: StagedDownload,
        incoming: Inventory,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> io::Result<()>,
        arm: impl FnOnce(u32) -> io::Result<()>,
        recovery_prefix: Option<&str>,
    ) -> io::Result<ReviewedFilePublication> {
        require_running(stopped)?;
        authorize()?;
        let _ancestors = local_tree::hold_reviewed_file_ancestors(self.parent_path())?;
        self.require_parent()?;
        self.require_named(&self.witness.stamp)?;
        if stage.destination() != self.path || !single_file(incoming, self.max_bytes) {
            return Err(changed());
        }
        let input = FileWitness::capture_bounded(stage.writer()?, stopped, self.max_bytes)?;
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
            let final_file = FileWitness::capture_bounded(file, stopped, self.max_bytes)?;
            if final_file.inventory != incoming || !final_file.stamp.same_identity(&input.stamp) {
                return Err(changed().into());
            }
            file_before_publish.replace(Some(final_file));
            guard()?;
            Ok(())
        };
        let after = |parent: &Directory, name: &OsStr, file: &File| {
            local_tree::sync_reviewed_tree_directory(parent)?;
            let witness = FileWitness::capture_bounded(file, &|| false, self.max_bytes)?;
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
                backup_prefix: recovery_prefix,
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
                    successor: Box::new(receipt),
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
        require_bound_parent(self.parent_path(), &self.parent, &self.parent_stamp)
    }

    fn require_named(&self, expected: &ReviewedStamp) -> io::Result<()> {
        require_named_file(&self.parent, self.name(), expected)
    }

    fn snapshot(&self, file: &FileWitness) -> LocalFileSnapshot {
        file_snapshot(&self.path, &self.parent, &self.parent_stamp, file)
    }
}

fn require_bound_parent(
    path: &Path,
    expected: &Directory,
    stamp: &ReviewedStamp,
) -> io::Result<()> {
    let parent = local_tree::open_reviewed_tree_parent(path)?;
    if expected.same_as(&parent)? && local_tree::reviewed_tree_parent_stamp(&parent)? == *stamp {
        Ok(())
    } else {
        Err(changed())
    }
}

fn require_named_file(parent: &Directory, name: &OsStr, stamp: &ReviewedStamp) -> io::Result<()> {
    let Entry::File(file) = local_tree::entry(parent, name)? else {
        return Err(changed());
    };
    if local_tree::reviewed_file_stamp(&file)? == *stamp {
        Ok(())
    } else {
        Err(changed())
    }
}

fn file_snapshot(
    path: &Path,
    directory: &Directory,
    parent: &ReviewedStamp,
    file: &FileWitness,
) -> LocalFileSnapshot {
    let mut identity = Fingerprint(Sha256::new_with_prefix(b"kortty-local-rsync-file-id-v1\0"));
    file.stamp.hash_identity_into(&mut identity);
    let mut proof = path_fingerprint(b"kortty-local-rsync-file-v1\0", path);
    parent.hash_into(&mut proof);
    file.stamp.hash_into(&mut proof);
    proof.write(&file.inventory.digest);
    let alias = VacantLocalFileAlias::capture(
        directory,
        parent,
        path.file_name().expect("checked receiver filename"),
    );
    LocalFileSnapshot {
        inventory: file.inventory,
        fingerprint: proof.0.finalize().into(),
        root_identity: identity.0.finalize().into(),
        alias: Some(alias),
        location: Some(LocalTreeLocation::new(
            alias,
            parent.volume().mount_identity(),
        )),
        published_from: None,
    }
}

fn path_fingerprint(domain: &[u8], path: &Path) -> Fingerprint {
    let mut proof = Fingerprint(Sha256::new_with_prefix(domain));
    let path = path.as_os_str().as_encoded_bytes();
    proof.write_usize(path.len());
    proof.write(path);
    proof
}

pub(crate) struct FileWitness {
    pub(crate) stamp: ReviewedStamp,
    digest: [u8; 32],
    pub(crate) inventory: Inventory,
}

impl FileWitness {
    pub(crate) fn snapshot(
        &self,
        path: &Path,
        directory: &Directory,
        parent: &ReviewedStamp,
    ) -> LocalFileSnapshot {
        file_snapshot(path, directory, parent, self)
    }

    pub(crate) fn capture(file: &File, stopped: &impl Fn() -> bool) -> io::Result<Self> {
        Self::capture_bounded(file, stopped, MAX_BYTES)
    }

    fn capture_bounded(
        mut file: &File,
        stopped: &impl Fn() -> bool,
        max_bytes: u64,
    ) -> io::Result<Self> {
        require_running(stopped)?;
        require_single_link(file)?;
        let stamp = local_tree::reviewed_file_stamp(file)?;
        let bytes = file.metadata()?.len();
        if bytes > max_bytes {
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
        Ok(Self {
            stamp,
            digest,
            inventory: Manifest::regular_file(bytes, digest),
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

fn single_file(inventory: Inventory, max_bytes: u64) -> bool {
    inventory.bytes <= max_bytes && inventory.entries == 1 && inventory.files == 1
}

fn checked_location(path: &Path) -> io::Result<(&Path, &OsStr)> {
    let (parent, name) = path.parent().zip(path.file_name()).ok_or_else(changed)?;
    reportable(
        &parent
            .join(".kortty-00000000000000000000000000000000.kortty-part")
            .join(name),
    )?;
    Ok((parent, name))
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

    fn vacant_fixture() -> (Profile, PathBuf) {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = Directory::open_path(&root).unwrap();
        let workspace = OsStr::new("workspace");
        drop(local_tree::create_reviewed_tree_stage(&directory, workspace).unwrap());
        let destination = root.join(workspace).join("receiver.txt");
        (profile, destination)
    }

    fn fixture(bytes: &[u8]) -> (Profile, PathBuf) {
        let (profile, destination) = vacant_fixture();
        write_fixture_file(&destination, bytes);
        (profile, destination)
    }

    fn write_fixture_file(path: &Path, bytes: &[u8]) {
        let parent = Directory::open_path(path.parent().unwrap()).unwrap();
        let mut file =
            local_tree::create_reviewed_tree_file(&parent, path.file_name().unwrap()).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn staged(stage: StagedDownload, bytes: &[u8]) -> (StagedDownload, Inventory) {
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
        let (stage, incoming) = staged(receiver.stage().unwrap(), b"new bytes");
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
            *successor,
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
        assert!(ReviewedFileReceiver::prepare(&destination, *successor, &|| false).is_err());
    }

    #[test]
    fn checked_local_file_restores_or_retains_when_publication_loses_the_name() {
        for occupant in [false, true] {
            let (_fixture, destination) = fixture(b"old");
            let old = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
            let receiver = ReviewedFileReceiver::prepare(&destination, old, &|| false).unwrap();
            let (stage, incoming) = staged(receiver.stage().unwrap(), b"new");
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
        let (stage, _) = staged(receiver.stage().unwrap(), b"new");
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

    #[test]
    fn checked_vacant_file_returns_a_receipt_for_the_next_replacement() {
        let (_fixture, destination) = vacant_fixture();
        let vacant = VacantLocalFileReceiver::inspect(&destination, &|| false).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTimesExt as _;
            use std::time::{Duration, UNIX_EPOCH};

            let previous = File::create(&destination).unwrap();
            let earlier = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
            previous
                .set_times(fs::FileTimes::new().set_created(earlier))
                .unwrap();
            drop(previous);
            fs::remove_file(&destination).unwrap();
        }
        let receiver = VacantLocalFileReceiver::prepare(&destination, vacant, &|| false).unwrap();
        let (stage, incoming) = staged(receiver.stage().unwrap(), b"first");
        let result = receiver
            .publish(stage, incoming, &|| false, &|| Ok(()), |removed| {
                assert_eq!(removed, 0);
                assert!(!destination.exists());
                Ok(())
            })
            .unwrap();
        let VacantLocalFilePublication::Published(successor) = result else {
            panic!("the checked vacant file must publish");
        };
        assert_eq!(successor.inventory, incoming);
        assert_eq!(vacant.alias(), successor.alias());
        assert_eq!(fs::read(&destination).unwrap(), b"first");
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
        assert!(VacantLocalFileReceiver::inspect(&destination, &|| false).is_err());

        let receiver = ReviewedFileReceiver::prepare(&destination, *successor, &|| false).unwrap();
        let (stage, incoming) = staged(receiver.stage().unwrap(), b"second");
        let result = receiver
            .publish(stage, incoming, &|| false, &|| Ok(()), |_| Ok(()))
            .unwrap();
        let ReviewedFilePublication::Published { recovery, .. } = result else {
            panic!("the exact first publication must be replaceable");
        };
        assert_eq!(fs::read(&recovery).unwrap(), b"first");
        assert_eq!(fs::read(&destination).unwrap(), b"second");
    }

    #[test]
    fn checked_vacant_file_leaves_an_occupant_created_during_arming_untouched() {
        let (_fixture, destination) = vacant_fixture();
        let vacant = VacantLocalFileReceiver::inspect(&destination, &|| false).unwrap();
        let receiver = VacantLocalFileReceiver::prepare(&destination, vacant, &|| false).unwrap();
        let (stage, incoming) = staged(receiver.stage().unwrap(), b"incoming");
        assert!(
            receiver
                .publish(stage, incoming, &|| false, &|| Ok(()), |_| {
                    fs::write(&destination, b"intervening file")
                })
                .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"intervening file");
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn checked_vacant_file_rejects_a_changed_path_or_parent() {
        let (_fixture, destination) = vacant_fixture();
        let vacant = VacantLocalFileReceiver::inspect(&destination, &|| false).unwrap();
        assert!(
            VacantLocalFileReceiver::prepare(
                &destination.with_file_name("another.txt"),
                vacant,
                &|| false,
            )
            .is_err()
        );
        let parent = destination.parent().unwrap();
        fs::rename(parent, parent.with_file_name("previous workspace")).unwrap();
        let root = Directory::open_path(parent.parent().unwrap()).unwrap();
        drop(local_tree::create_reviewed_tree_stage(&root, parent.file_name().unwrap()).unwrap());
        assert!(VacantLocalFileReceiver::prepare(&destination, vacant, &|| false).is_err());
        assert!(VacantLocalFileReceiver::inspect(&destination, &|| false).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn checked_vacant_file_refuses_a_dangling_link() {
        let (_fixture, destination) = vacant_fixture();
        std::os::unix::fs::symlink("missing-target", &destination).unwrap();
        assert!(VacantLocalFileReceiver::inspect(&destination, &|| false).is_err());
    }

    #[test]
    fn local_file_review_accepts_legacy_snapshots_without_an_alias() {
        let (_fixture, destination) = vacant_fixture();
        let vacant = VacantLocalFileReceiver::inspect(&destination, &|| false).unwrap();
        let mut previous = serde_json::to_value(vacant).unwrap();
        previous.as_object_mut().unwrap().remove("alias").unwrap();
        previous
            .as_object_mut()
            .unwrap()
            .remove("location")
            .unwrap();
        let legacy: VacantLocalFileSnapshot = serde_json::from_value(previous.clone()).unwrap();
        assert!(legacy.alias().is_none());
        assert_eq!(serde_json::to_value(legacy).unwrap(), previous);
        VacantLocalFileReceiver::prepare(&destination, legacy, &|| false).unwrap();

        write_fixture_file(&destination, b"existing");
        let existing = ReviewedFileReceiver::inspect(&destination, &|| false).unwrap();
        assert!(existing.alias().unwrap().aliases(vacant.alias().unwrap()));
        let mut previous = serde_json::to_value(existing).unwrap();
        previous.as_object_mut().unwrap().remove("alias").unwrap();
        previous
            .as_object_mut()
            .unwrap()
            .remove("location")
            .unwrap();
        let legacy: LocalFileSnapshot = serde_json::from_value(previous.clone()).unwrap();
        assert!(legacy.alias().is_none());
        assert_eq!(serde_json::to_value(legacy).unwrap(), previous);
        ReviewedFileReceiver::prepare(&destination, legacy, &|| false).unwrap();
    }

    #[test]
    fn local_file_alias_binding_checks_physical_parent_and_native_name_rules() {
        let alias = |parent, name| VacantLocalFileAlias { parent, name };
        let first = alias([1; 32], AliasName::FoldedAscii([2; 32]));
        let same = alias([1; 32], AliasName::FoldedAscii([2; 32]));
        let other = alias([1; 32], AliasName::FoldedAscii([3; 32]));
        let indeterminate = alias([1; 32], AliasName::Indeterminate);
        let elsewhere = alias([2; 32], AliasName::Indeterminate);
        assert!(first.aliases(&same));
        assert!(!first.aliases(&other));
        assert!(first.aliases(&indeterminate));
        assert!(!first.aliases(&elsewhere));

        let (_fixture, destination) = vacant_fixture();
        let exact = VacantLocalFileReceiver::inspect(&destination, &|| false).unwrap();
        let upper =
            VacantLocalFileReceiver::inspect(&destination.with_file_name("RECEIVER.TXT"), &|| {
                false
            })
            .unwrap();
        let exact = exact.alias().unwrap();
        let upper = upper.alias().unwrap();
        assert_eq!(exact.parent, upper.parent);
        assert_eq!(
            exact.aliases(upper),
            matches!(
                exact.name,
                AliasName::FoldedAscii(_) | AliasName::Indeterminate
            )
        );
    }
}
