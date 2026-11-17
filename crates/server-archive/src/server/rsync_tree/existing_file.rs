//! Guarded publication of an individually reviewed rsync receiver file.

use std::fs::FileTimes;
use std::io::Seek as _;
use std::os::fd::AsRawFd as _;

use kortty_server_archive::MAX_ARCHIVE_BYTES;
use kortty_server_archive::protocol::CreatedPayload;
use kortty_server_archive::rsync_tree::{ExistingTree, MAX_BYTES};
use rustix::fs::{FlockOperation, StatxFlags, flock, statx};

use super::super::draft_replace::{mount_id, read_xattrs};
use super::existing::{Published, Recovery, copy_metadata};
use super::*;

pub(in crate::server) const FILE: &str = "file";
const ORIGINAL: &str = "original";

pub(in crate::server) struct Target<'a> {
    destination: &'a str,
    path: &'a str,
    name: &'a str,
    parent: Directory,
    parent_attributes: BTreeMap<String, Vec<u8>>,
    mount: u64,
    max_bytes: u64,
    original: CheckedFile,
}

pub(in crate::server) struct CheckedFile {
    file: File,
    stamp: Stamp,
    inventory: Inventory,
    digest: [u8; 32],
    attributes: BTreeMap<String, Vec<u8>>,
}

/// A checked file slot that may be published once without replacing an occupant.
pub(in crate::server) struct VacantTarget<'a> {
    pub(super) destination: &'a str,
    pub(super) path: &'a str,
    pub(super) name: &'a str,
    parent: Directory,
    parent_attributes: BTreeMap<String, Vec<u8>>,
    pub(super) mount: u64,
    birth: (i64, u32),
}

impl<'a> VacantTarget<'a> {
    pub(in crate::server) fn open_at(destination: &'a str, parent: Directory) -> io::Result<Self> {
        let (path, name) = absolute_file(destination)?;
        if parent.stat.st_uid != geteuid().as_raw() || parent.stat.st_mode & 0o7022 != 0 {
            return Err(changed());
        }
        let target = Self {
            destination,
            path,
            name,
            parent_attributes: read_xattrs(&parent.fd)?,
            mount: mount_id(&parent.fd)?,
            birth: parent_birth(&parent)?,
            parent,
        };
        target.require_vacant()?;
        Ok(target)
    }

    pub(in crate::server) fn parent(&self) -> &Directory {
        &self.parent
    }

    pub(in crate::server) fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint_with_domain(b"kortty-rsync-first-recurring-file-v1\0")
    }

    pub(super) fn fingerprint_with_domain(&self, domain: &[u8]) -> [u8; 32] {
        let mut hash = Sha256::new_with_prefix(domain);
        hash_parent(
            &mut hash,
            self.destination,
            &self.parent,
            self.mount,
            &self.parent_attributes,
        );
        hash.update(self.birth.0.to_be_bytes());
        hash.update(self.birth.1.to_be_bytes());
        hash.finalize().into()
    }

    pub(in crate::server) fn require_vacant(&self) -> io::Result<()> {
        self.verify_parent()?;
        require_absent(&self.parent.fd, self.name)
    }

    pub(in crate::server) fn prepare(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
    ) -> io::Result<CheckedFile> {
        self.prepare_bounded(stage, expected, MAX_BYTES)
    }

    pub(in crate::server) fn prepare_transfer(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
    ) -> io::Result<CheckedFile> {
        self.prepare_bounded(
            stage,
            expected,
            kortty_server_archive::privileged_file_replace::MAX_BYTES,
        )
    }

    fn prepare_bounded(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
        limit: u64,
    ) -> io::Result<CheckedFile> {
        self.require_vacant()?;
        self.verify_stage(stage)?;
        require_file_payload(stage)?;
        let file = CheckedFile::open_bounded(&stage.payload, FILE, self.mount, limit)?;
        if file.inventory != expected || file.stamp.mode & 0o7777 != 0o600 {
            return Err(invalid(
                "The received private rsync file differs from the verified input",
            ));
        }
        fsync(&file.file)?;
        fsync(&stage.payload.fd)?;
        fsync(&stage.directory.fd)?;
        Ok(file)
    }

    pub(in crate::server) fn publish(
        &self,
        stage: &UploadStage<'_>,
        file: &CheckedFile,
        outcome_unknown: &mut bool,
        permit: impl Fn() -> io::Result<()>,
    ) -> io::Result<ExistingTree> {
        let _ancestors = lock_receiver_ancestors(self.path, &self.parent)?;
        flock(&file.file, FlockOperation::NonBlockingLockExclusive)?;
        self.verify_stage(stage)?;
        file.require_payload(stage, true)?;
        permit()?;
        self.require_vacant()?;
        self.verify_stage(stage)?;
        file.require_payload(stage, false)?;
        match renameat_with(
            &stage.payload.fd,
            FILE,
            &self.parent.fd,
            self.name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => *outcome_unknown = true,
            Err(Errno::EXIST | Errno::NOTEMPTY) => {
                return Err(invalid("The reviewed server destination already exists"));
            }
            Err(error) => {
                *outcome_unknown = file.require_at(&stage.payload, FILE, false).is_err();
                return Err(error.into());
            }
        }
        let renamed = file.require_at(&self.parent, self.name, true)?;
        fsync(&stage.payload.fd)?;
        fsync(&self.parent.fd)?;
        permit()?;
        self.verify_parent()?;
        self.parent.require_child(self.name, &fstat(&file.file)?)?;
        if !renamed.matches(&file.file, false)? || read_xattrs(&file.file)? != file.attributes {
            return Err(changed());
        }
        Ok(file_proof(
            self.destination,
            &self.parent,
            self.mount,
            &self.parent_attributes,
            file,
            &renamed,
        ))
    }

    pub(super) fn verify_parent(&self) -> io::Result<()> {
        let current = checked_parent(self.path, &self.parent, self.mount, &self.parent_attributes)?;
        if parent_birth(&current)? != self.birth {
            return Err(changed());
        }
        Ok(())
    }

    pub(super) fn verify_stage(&self, stage: &UploadStage<'_>) -> io::Result<()> {
        stage.verify(self.path)?;
        if !same_node(&stage.parent.stat, &self.parent.stat)
            || mount_id(&stage.directory.fd)? != self.mount
            || mount_id(&stage.payload.fd)? != self.mount
        {
            return Err(changed());
        }
        Ok(())
    }
}

impl<'a> Target<'a> {
    pub(in crate::server) fn open_at(destination: &'a str, parent: Directory) -> io::Result<Self> {
        Self::open_bounded_at(destination, parent, MAX_BYTES)
    }

    pub(in crate::server) fn open_archive_at(
        destination: &'a str,
        parent: Directory,
    ) -> io::Result<Self> {
        Self::open_bounded_at(destination, parent, MAX_ARCHIVE_BYTES)
    }

    pub(in crate::server) fn open_transfer_at(
        destination: &'a str,
        parent: Directory,
    ) -> io::Result<Self> {
        Self::open_transfer_limited(
            destination,
            parent,
            kortty_server_archive::privileged_file_replace::MAX_BYTES,
        )
    }

    pub(in crate::server) fn open_transfer_limited(
        destination: &'a str,
        parent: Directory,
        remaining: u64,
    ) -> io::Result<Self> {
        let limit = kortty_server_archive::privileged_file_replace::MAX_BYTES;
        let mut target = Self::open_bounded_at(destination, parent, remaining.min(limit))?;
        target.max_bytes = limit;
        Ok(target)
    }

    fn open_bounded_at(destination: &'a str, parent: Directory, max: u64) -> io::Result<Self> {
        let (path, name) = absolute_file(destination)?;
        if parent.stat.st_uid != geteuid().as_raw() || parent.stat.st_mode & 0o7022 != 0 {
            return Err(changed());
        }
        let mount = mount_id(&parent.fd)?;
        let parent_attributes = read_xattrs(&parent.fd)?;
        let original = CheckedFile::open_bounded(&parent, name, mount, max)?;
        let target = Self {
            destination,
            path,
            name,
            parent,
            parent_attributes,
            mount,
            max_bytes: max,
            original,
        };
        target.require_current()?;
        Ok(target)
    }

    pub(in crate::server) fn review(&self) -> ExistingTree {
        self.proof(&self.original, &self.original.stamp)
    }

    pub(in crate::server) fn content(
        &self,
    ) -> kortty_server_archive::privileged_file_replace::Payload {
        kortty_server_archive::privileged_file_replace::Payload {
            bytes: self.original.inventory.bytes,
            sha256: self.original.digest,
        }
    }

    pub(in crate::server) fn parent(&self) -> &Directory {
        &self.parent
    }

    pub(in crate::server) fn destination(&self) -> &str {
        self.destination
    }

    pub(in crate::server) fn original_archive(&self) -> io::Result<(File, CreatedPayload)> {
        self.require_current()?;
        let payload = self.original.archive_payload()?;
        Ok((self.original.file.try_clone()?, payload))
    }

    pub(in crate::server) fn retain_archive_metadata(&self, file: &File) -> io::Result<()> {
        self.require_current()?;
        let modified = file.metadata()?.modified()?;
        copy_metadata(&self.original.file, file)?;
        file.set_times(FileTimes::new().set_modified(modified))?;
        self.require_current()
    }

    fn proof(&self, file: &CheckedFile, stamp: &Stamp) -> ExistingTree {
        file_proof(
            self.destination,
            &self.parent,
            self.mount,
            &self.parent_attributes,
            file,
            stamp,
        )
    }

    pub(in crate::server) fn prepare_successor(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
    ) -> io::Result<CheckedFile> {
        self.require_current()?;
        self.verify_stage(stage)?;
        require_file_payload(stage)?;
        let received = CheckedFile::open_bounded(&stage.payload, FILE, self.mount, self.max_bytes)?;
        if received.inventory != expected || received.stamp.mode & 0o7777 != 0o600 {
            return Err(invalid(
                "The received rsync file differs from the reviewed input",
            ));
        }
        copy_metadata(&self.original.file, &received.file)?;
        let successor = CheckedFile::inspect_bounded(received.file, self.max_bytes)?;
        successor.require_payload(stage, false)?;
        if successor.inventory != expected
            || successor.stamp.uid != self.original.stamp.uid
            || successor.stamp.gid != self.original.stamp.gid
            || successor.stamp.mode != self.original.stamp.mode
            || successor.stamp.mtime != self.original.stamp.mtime
            || successor.stamp.mtime_ns != self.original.stamp.mtime_ns
            || successor.attributes != self.original.attributes
        {
            return Err(invalid(
                "The private rsync file cannot retain the reviewed metadata",
            ));
        }
        self.require_current()?;
        fsync(&stage.payload.fd)?;
        fsync(&stage.directory.fd)?;
        Ok(successor)
    }

    pub(in crate::server) fn commit_checked(
        &self,
        stage: &mut UploadStage<'_>,
        successor: &CheckedFile,
        recurring: bool,
        recovery: &mut Recovery,
        permit: impl Fn() -> io::Result<()>,
        confirm: impl FnOnce(&str) -> io::Result<bool>,
    ) -> io::Result<Option<Published>> {
        let _ancestors = lock_receiver_ancestors(self.path, &self.parent)?;
        match flock(
            &self.original.file,
            FlockOperation::NonBlockingLockExclusive,
        ) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                return Err(invalid(
                    "Another rsync publication holds the reviewed receiver",
                ));
            }
            Err(error) => return Err(error.into()),
        }
        self.verify_stage(stage)?;
        if recurring {
            flock(&successor.file, FlockOperation::NonBlockingLockExclusive)?;
        }
        successor.require_payload(stage, recurring)?;
        self.require_current()?;
        let path = format!(
            "{}/{}/{ORIGINAL}",
            self.path.trim_end_matches('/'),
            stage.name
        );
        permit()?;
        let moved = renameat_with(
            &self.parent.fd,
            self.name,
            &stage.directory.fd,
            ORIGINAL,
            RenameFlags::NOREPLACE,
        );
        if let Err(error) = moved {
            // Some filesystems may report a failure even after moving the entry.
            if !absent(&stage.directory.fd, ORIGINAL).unwrap_or(false) {
                stage.owned = false;
                recovery.uncertain = true;
                recovery.path = self.checked_recovery(stage, &path);
            }
            return Err(error.into());
        }
        stage.owned = false;
        recovery.uncertain = true;
        recovery.path = self.checked_recovery(stage, &path);
        let displaced = statat(&stage.directory.fd, ORIGINAL, AtFlags::SYMLINK_NOFOLLOW)?;
        let result = (|| {
            self.original.require_at(&stage.directory, ORIGINAL, true)?;
            fsync(&stage.directory.fd)?;
            fsync(&self.parent.fd)?;
            let current_recovery = self.checked_recovery(stage, &path).ok_or_else(changed)?;
            if !confirm(&current_recovery)? {
                return Ok(None);
            }
            self.verify_stage(stage)?;
            successor.require_payload(stage, recurring)?;
            self.original.require_at(&stage.directory, ORIGINAL, true)?;
            self.verify_stage(stage)?;
            successor.require_payload(stage, false)?;
            self.verify_parent()?;
            permit()?;
            renameat_with(
                &stage.payload.fd,
                FILE,
                &self.parent.fd,
                self.name,
                RenameFlags::NOREPLACE,
            )?;
            stage.published = true;
            let renamed = successor.require_at(&self.parent, self.name, true)?;
            stage.directory.require_child(ORIGINAL, &displaced)?;
            fsync(&stage.payload.fd)?;
            stage
                .directory
                .require_child(PAYLOAD, &stage.payload.stat)?;
            unlinkat(&stage.directory.fd, PAYLOAD, AtFlags::REMOVEDIR)?;
            fsync(&stage.directory.fd)?;
            fsync(&self.parent.fd)?;
            let receiver = if recurring {
                permit()?;
                let proof = self.proof(successor, &renamed);
                self.parent
                    .require_child(self.name, &fstat(&successor.file)?)?;
                if !renamed.matches(&successor.file, false)? {
                    return Err(changed());
                }
                Some(proof)
            } else {
                None
            };
            Ok(Some(Published {
                recovery: self.checked_recovery(stage, &path).ok_or_else(changed)?,
                receiver,
            }))
        })();
        if matches!(result, Ok(Some(_))) {
            return result;
        }
        let restored = if stage.published {
            Err(invalid("The rsync file may already have been published"))
        } else {
            self.restore(stage, &displaced).inspect(|()| {
                stage.owned = true;
                *recovery = Recovery::default();
            })
        };
        if recovery.uncertain {
            recovery.path = self.checked_recovery(stage, &path);
        }
        result?;
        restored?;
        Ok(None)
    }

    pub(in crate::server) fn require_current(&self) -> io::Result<()> {
        self.verify_parent()?;
        self.original
            .require_at(&self.parent, self.name, false)
            .map(|_| ())
    }

    fn verify_stage(&self, stage: &UploadStage<'_>) -> io::Result<()> {
        stage.verify(self.path)?;
        let directory = self.verify_recovery_directory(stage)?;
        let payload = directory.child(PAYLOAD)?;
        if !same_node(&stage.payload.stat, &payload.stat) || mount_id(&payload.fd)? != self.mount {
            return Err(changed());
        }
        Ok(())
    }

    fn verify_parent(&self) -> io::Result<()> {
        checked_parent(self.path, &self.parent, self.mount, &self.parent_attributes).map(|_| ())
    }

    fn checked_recovery(&self, stage: &UploadStage<'_>, path: &str) -> Option<String> {
        (self.verify_parent().is_ok()
            && self.verify_recovery_directory(stage).is_ok()
            && fstat(&self.original.file)
                .is_ok_and(|original| stage.directory.require_child(ORIGINAL, &original).is_ok()))
        .then(|| path.to_owned())
    }

    fn restore(&self, stage: &UploadStage<'_>, displaced: &Stat) -> io::Result<()> {
        self.verify_parent()?;
        self.verify_recovery_directory(stage)?;
        stage.directory.require_child(ORIGINAL, displaced)?;
        renameat_with(
            &stage.directory.fd,
            ORIGINAL,
            &self.parent.fd,
            self.name,
            RenameFlags::NOREPLACE,
        )?;
        self.parent.require_child(self.name, displaced)?;
        fsync(&stage.directory.fd)?;
        fsync(&self.parent.fd)?;
        self.verify_parent()
    }

    fn verify_recovery_directory(&self, stage: &UploadStage<'_>) -> io::Result<Directory> {
        let directory = self.parent.child(&stage.name)?;
        if !same_node(&stage.directory.stat, &directory.stat)
            || mount_id(&directory.fd)? != self.mount
        {
            return Err(changed());
        }
        require_private(&directory.fd, 0o700)?;
        Ok(directory)
    }
}

fn checked_parent(
    path: &str,
    parent: &Directory,
    mount: u64,
    attributes: &BTreeMap<String, Vec<u8>>,
) -> io::Result<Directory> {
    let current = Directory::open_parent(path)?;
    if !same_node(&parent.stat, &current.stat)
        || current.stat.st_uid != parent.stat.st_uid
        || current.stat.st_gid != parent.stat.st_gid
        || current.stat.st_mode != parent.stat.st_mode
        || mount_id(&current.fd)? != mount
        || read_xattrs(&current.fd)? != *attributes
    {
        return Err(changed());
    }
    Ok(current)
}

fn parent_birth(parent: &Directory) -> io::Result<(i64, u32)> {
    let stat = statx(&parent.fd, "", AtFlags::EMPTY_PATH, StatxFlags::BTIME)?;
    let birth = (stat.stx_btime.tv_sec, stat.stx_btime.tv_nsec);
    if stat.stx_mask & StatxFlags::BTIME.bits() == 0 || birth == (0, 0) {
        return Err(invalid(
            "The reviewed rsync parent needs a stable creation timestamp",
        ));
    }
    Ok(birth)
}

fn hash_parent(
    hash: &mut Sha256,
    destination: &str,
    parent: &Directory,
    mount: u64,
    attributes: &BTreeMap<String, Vec<u8>>,
) {
    hash.update((destination.len() as u32).to_be_bytes());
    hash.update(destination.as_bytes());
    hash.update(parent.stat.st_dev.to_be_bytes());
    hash.update(parent.stat.st_ino.to_be_bytes());
    hash.update(parent.stat.st_mode.to_be_bytes());
    hash.update(parent.stat.st_uid.to_be_bytes());
    hash.update(parent.stat.st_gid.to_be_bytes());
    hash.update(mount.to_be_bytes());
    hash_attributes(hash, attributes);
}

pub(in crate::server) fn directory_fingerprint(
    path: &str,
    directory: &Directory,
) -> io::Result<[u8; 32]> {
    let mount = mount_id(&directory.fd)?;
    let birth = parent_birth(directory)?;
    let attributes = read_xattrs(&directory.fd)?;
    let mut hash = Sha256::new_with_prefix(b"kortty-protected-directory-v1\0");
    hash_parent(&mut hash, path, directory, mount, &attributes);
    hash.update(birth.0.to_be_bytes());
    hash.update(birth.1.to_be_bytes());
    Ok(hash.finalize().into())
}

fn file_proof(
    destination: &str,
    parent: &Directory,
    mount: u64,
    attributes: &BTreeMap<String, Vec<u8>>,
    file: &CheckedFile,
    stamp: &Stamp,
) -> ExistingTree {
    let mut hash = Sha256::new_with_prefix(b"kortty-rsync-existing-file-v1\0");
    hash_parent(&mut hash, destination, parent, mount, attributes);
    stamp.hash_into(&mut hash);
    hash.update(file.inventory.digest);
    hash_attributes(&mut hash, &file.attributes);
    ExistingTree {
        inventory: file.inventory,
        fingerprint: hash.finalize().into(),
    }
}

impl CheckedFile {
    fn open_bounded(parent: &Directory, name: &str, mount: u64, max: u64) -> io::Result<Self> {
        let pin = openat(
            &parent.fd,
            name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let held = File::from(pin);
        let metadata = held.metadata()?;
        let original = Stamp::of(&metadata);
        if !metadata.is_file()
            || original.uid != geteuid().as_raw()
            || original.links != 1
            || original.mode & 0o7022 != 0
            || original.size > max
            || mount_id(&held)? != mount
        {
            return Err(changed());
        }
        let file = File::from(open(
            format!("/proc/self/fd/{}", held.as_raw_fd()),
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let checked = Self::inspect_bounded(file, max)?;
        if checked.stamp != original {
            return Err(changed());
        }
        checked.require_at(parent, name, false)?;
        Ok(checked)
    }

    pub(in crate::server) fn generated_archive(
        stage: &UploadStage<'_>,
        file: File,
        expected: CreatedPayload,
    ) -> io::Result<Self> {
        expected.validate()?;
        require_file_payload(stage)?;
        let stat = fstat(&file)?;
        stage.payload.require_child(FILE, &stat)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != geteuid().as_raw()
            || stat.st_nlink != 1
            || stat.st_mode & 0o7022 != 0
            || u64::try_from(stat.st_size).ok() != Some(expected.bytes)
            || mount_id(&file)? != mount_id(&stage.payload.fd)?
        {
            return Err(changed());
        }
        let checked = Self::inspect_bounded(file, MAX_ARCHIVE_BYTES)?;
        if checked.archive_payload()? != expected {
            return Err(changed());
        }
        checked.require_payload(stage, false)?;
        fsync(&checked.file)?;
        fsync(&stage.payload.fd)?;
        fsync(&stage.directory.fd)?;
        Ok(checked)
    }

    fn archive_payload(&self) -> io::Result<CreatedPayload> {
        let payload = CreatedPayload {
            bytes: self.stamp.size,
            sha256: self.digest,
        };
        payload.validate()?;
        Ok(payload)
    }

    fn inspect_bounded(file: File, max: u64) -> io::Result<Self> {
        let stamp = Stamp::of(&file.metadata()?);
        if stamp.size > max {
            return Err(invalid("The reviewed file exceeds its size limit"));
        }
        let attributes = read_xattrs(&file)?;
        let (inventory, digest) = file_content(&file, stamp.size)?;
        if !stamp.matches(&file, false)? {
            return Err(changed());
        }
        Ok(Self {
            file,
            stamp,
            inventory,
            digest,
            attributes,
        })
    }

    fn require_at(&self, parent: &Directory, name: &str, moved: bool) -> io::Result<Stamp> {
        parent.require_child(name, &fstat(&self.file)?)?;
        let current = Stamp::of(&self.file.metadata()?);
        if !self.stamp.matches(&self.file, moved)?
            || (moved && file_inventory(&self.file, self.stamp.size)? != self.inventory)
            || read_xattrs(&self.file)? != self.attributes
            || !current.matches(&self.file, false)?
        {
            return Err(changed());
        }
        parent.require_child(name, &fstat(&self.file)?)?;
        Ok(current)
    }

    fn require_payload(&self, stage: &UploadStage<'_>, hash_bytes: bool) -> io::Result<()> {
        require_file_payload(stage)?;
        self.require_at(&stage.payload, FILE, false)?;
        if hash_bytes {
            if file_inventory(&self.file, self.stamp.size)? != self.inventory {
                return Err(changed());
            }
            require_file_payload(stage)?;
            self.require_at(&stage.payload, FILE, false)?;
        }
        Ok(())
    }
}

fn require_file_payload(stage: &UploadStage<'_>) -> io::Result<()> {
    if !names(&stage.payload, 1)?
        .iter()
        .map(String::as_str)
        .eq([FILE])
    {
        return Err(invalid(
            "The private rsync payload must contain exactly one file",
        ));
    }
    Ok(())
}

fn file_inventory(file: &File, bytes: u64) -> io::Result<Inventory> {
    file_content(file, bytes).map(|(inventory, _)| inventory)
}

fn file_content(mut file: &File, bytes: u64) -> io::Result<(Inventory, [u8; 32])> {
    file.rewind()?;
    let digest = hash(file, bytes)?;
    Ok((Manifest::regular_file(bytes, digest), digest))
}

fn hash_attributes(hash: &mut Sha256, attributes: &BTreeMap<String, Vec<u8>>) {
    hash.update((attributes.len() as u32).to_be_bytes());
    for (name, value) in attributes {
        hash.update((name.len() as u32).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update((value.len() as u32).to_be_bytes());
        hash.update(value);
    }
}

fn changed() -> io::Error {
    invalid("The reviewed rsync file changed or is unavailable")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs::{self, DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
    use std::path::PathBuf;

    use rustix::fs::{XattrFlags, fsetxattr, mkfifoat};

    use super::*;

    const OLD: &[u8] = b"prior file\n";
    const NEW: &[u8] = b"fresh file\n";

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("kortty-rsync-existing-file-{}", hex::encode(nonce)));
            DirBuilder::new().mode(0o700).create(&root).unwrap();
            DirBuilder::new()
                .mode(0o700)
                .create(root.join("receiver"))
                .unwrap();
            let fixture = Self(root);
            fs::write(fixture.destination(), OLD).unwrap();
            fs::set_permissions(fixture.destination(), Permissions::from_mode(0o640)).unwrap();
            fixture
        }

        fn destination(&self) -> PathBuf {
            self.0.join("receiver/file")
        }

        fn target<'a>(&self, destination: &'a str) -> io::Result<Target<'a>> {
            let parent = Directory::open_parent(self.0.join("receiver").to_str().unwrap())?;
            Target::open_at(destination, parent)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn candidate<'target>(target: &'target Target<'_>) -> (UploadStage<'target>, CheckedFile) {
        use std::io::Write as _;

        let stage = UploadStage::create(target.parent()).unwrap();
        let mut file = File::from(
            openat(
                &stage.payload.fd,
                FILE,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
                PRIVATE_FILE,
            )
            .unwrap(),
        );
        file.write_all(NEW).unwrap();
        file.sync_all().unwrap();
        let expected = file_inventory(&file, NEW.len() as u64).unwrap();
        let successor = target.prepare_successor(&stage, expected).unwrap();
        (stage, successor)
    }

    fn first_candidate<'target>(
        target: &'target VacantTarget<'_>,
    ) -> (UploadStage<'target>, CheckedFile) {
        use std::io::Write as _;

        let stage = UploadStage::create(target.parent()).unwrap();
        let mut file = File::from(
            openat(
                &stage.payload.fd,
                FILE,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
                PRIVATE_FILE,
            )
            .unwrap(),
        );
        file.write_all(NEW).unwrap();
        file.sync_all().unwrap();
        let expected = file_inventory(&file, NEW.len() as u64).unwrap();
        let successor = target.prepare(&stage, expected).unwrap();
        (stage, successor)
    }

    #[test]
    fn first_recurring_privileged_file_returns_the_exact_existing_receiver() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        fs::remove_file(&destination).unwrap();
        let path = destination.to_str().unwrap();
        let parent =
            || Directory::open_parent(fixture.0.join("receiver").to_str().unwrap()).unwrap();
        let vacant = VacantTarget::open_at(path, parent()).unwrap();
        let original = vacant.fingerprint();
        let (mut stage, file) = first_candidate(&vacant);
        assert!(!destination.exists());
        assert_eq!(
            VacantTarget::open_at(path, parent()).unwrap().fingerprint(),
            original
        );

        let mut published = false;
        let receipt = vacant
            .publish(&stage, &file, &mut published, || Ok(()))
            .unwrap();
        stage.discard().unwrap();
        assert!(published);
        assert_eq!(receipt, fixture.target(path).unwrap().review());
        assert_eq!(fs::read(&destination).unwrap(), NEW);
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(VacantTarget::open_at(path, parent()).is_err());
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn first_recurring_privileged_file_leaves_a_late_occupant_untouched() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        fs::remove_file(&destination).unwrap();
        let path = destination.to_str().unwrap();
        let parent = Directory::open_parent(fixture.0.join("receiver").to_str().unwrap()).unwrap();
        let vacant = VacantTarget::open_at(path, parent).unwrap();
        let (mut stage, file) = first_candidate(&vacant);
        let mut published = false;
        let error = vacant.publish(&stage, &file, &mut published, || {
            fs::write(&destination, OLD)
        });
        assert!(error.is_err());
        assert!(!published);
        stage.discard().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), OLD);
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn existing_privileged_rsync_rejects_stale_and_unsafe_file_receivers() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        let path = destination.to_str().unwrap();
        let target = fixture.target(path).unwrap();
        let first = target.review();
        fs::write(&destination, NEW).unwrap();
        assert!(target.require_current().is_err());
        let changed = fixture.target(path).unwrap().review();
        assert_ne!(first, changed);
        fs::rename(&destination, fixture.0.join("displaced")).unwrap();
        fs::write(&destination, NEW).unwrap();
        let substituted = fixture.target(path).unwrap().review();
        assert_eq!(changed.inventory, substituted.inventory);
        assert_ne!(changed.fingerprint, substituted.fingerprint);

        fs::hard_link(&destination, fixture.0.join("hardlink")).unwrap();
        assert!(fixture.target(path).is_err());
        fs::remove_file(&destination).unwrap();
        symlink(fixture.0.join("hardlink"), &destination).unwrap();
        assert!(fixture.target(path).is_err());
        fs::remove_file(&destination).unwrap();
        mkfifoat(&target.parent.fd, FILE, PRIVATE_FILE).unwrap();
        assert!(fixture.target(path).is_err());
        fs::remove_file(&destination).unwrap();
        let large = File::create(&destination).unwrap();
        large.set_len(MAX_BYTES + 1).unwrap();
        assert!(fixture.target(path).is_err());
    }

    #[test]
    fn existing_privileged_rsync_retains_original_and_checked_receiver_metadata() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        let original = File::open(&destination).unwrap();
        fsetxattr(
            &original,
            "user.kortty_test",
            b"retained",
            XattrFlags::empty(),
        )
        .unwrap();
        let target = fixture.target(destination.to_str().unwrap()).unwrap();
        let (mut stage, successor) = candidate(&target);
        let mut recovery = Recovery::default();
        let published = target
            .commit_checked(
                &mut stage,
                &successor,
                true,
                &mut recovery,
                || Ok(()),
                |path| {
                    assert!(!destination.exists());
                    assert_eq!(fs::read(path).unwrap(), OLD);
                    Ok(true)
                },
            )
            .unwrap()
            .unwrap();
        let fresh = fixture
            .target(destination.to_str().unwrap())
            .unwrap()
            .review();
        assert_eq!(published.receiver, Some(fresh));
        assert_eq!(fresh.inventory, successor.inventory);
        assert_ne!(fresh, target.review());
        let path = published.recovery;
        drop(stage);
        let metadata = destination.metadata().unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o640);
        assert_eq!(metadata.gid(), original.metadata().unwrap().gid());
        assert_eq!(fs::read(&destination).unwrap(), NEW);
        assert_eq!(
            read_xattrs(&File::open(&destination).unwrap()).unwrap()["user.kortty_test"],
            b"retained"
        );
        assert_eq!(fs::read(&path).unwrap(), OLD);
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            original.metadata().unwrap().ino()
        );
        let recovery_parent = std::path::Path::new(&path).parent().unwrap();
        assert!(!recovery_parent.join(PAYLOAD).exists());
    }

    #[test]
    fn recurring_privileged_rsync_refuses_to_attest_a_changed_published_file() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        let target = fixture.target(destination.to_str().unwrap()).unwrap();
        let (mut stage, successor) = candidate(&target);
        let mut recovery = Recovery::default();
        let checks = Cell::new(0);
        let result = target.commit_checked(
            &mut stage,
            &successor,
            true,
            &mut recovery,
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 3 {
                    fs::write(&destination, b"other file\n")?;
                }
                Ok(())
            },
            |_| Ok(true),
        );
        assert!(result.is_err());
        assert!(recovery.uncertain);
        assert_eq!(fs::read(recovery.path.unwrap()).unwrap(), OLD);
        assert_eq!(fs::read(&destination).unwrap(), b"other file\n");
    }

    #[test]
    fn existing_privileged_rsync_restores_after_abort_eof_or_policy_revocation() {
        for (confirm, revoke) in [
            (Ok(false), false),
            (Err(io::ErrorKind::UnexpectedEof), false),
            (Ok(true), true),
        ] {
            let fixture = Fixture::new();
            let destination = fixture.destination();
            let original = destination.metadata().unwrap().ino();
            let target = fixture.target(destination.to_str().unwrap()).unwrap();
            let (mut stage, successor) = candidate(&target);
            let private = fixture.0.join("receiver").join(&stage.name);
            let mut recovery = Recovery::default();
            let allowed = Cell::new(true);
            let result = target.commit_checked(
                &mut stage,
                &successor,
                true,
                &mut recovery,
                || allowed.get().then_some(()).ok_or_else(changed),
                |_| {
                    allowed.set(!revoke);
                    confirm.map_err(io::Error::from)
                },
            );
            if matches!(confirm, Ok(false)) {
                assert!(result.unwrap().is_none());
            } else {
                assert!(result.is_err());
            }
            assert!(!recovery.uncertain);
            assert!(recovery.path.is_none());
            assert_eq!(fs::read(&destination).unwrap(), OLD);
            assert_eq!(destination.metadata().unwrap().ino(), original);
            drop(stage);
            assert!(!private.exists());
        }
    }

    #[test]
    fn existing_privileged_rsync_retains_recovery_when_a_late_file_occupies_the_name() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        let original = destination.metadata().unwrap().ino();
        let target = fixture.target(destination.to_str().unwrap()).unwrap();
        let (mut stage, successor) = candidate(&target);
        let mut recovery = Recovery::default();
        let result = target.commit_checked(
            &mut stage,
            &successor,
            false,
            &mut recovery,
            || Ok(()),
            |_| {
                fs::write(&destination, b"separate late file")?;
                Ok(true)
            },
        );
        assert!(result.is_err());
        assert!(recovery.uncertain);
        let path = recovery.path.unwrap();
        drop(stage);
        assert_eq!(fs::read(&destination).unwrap(), b"separate late file");
        assert_eq!(fs::read(&path).unwrap(), OLD);
        assert_eq!(fs::metadata(path).unwrap().ino(), original);
    }

    #[test]
    fn existing_privileged_rsync_refuses_a_moved_parent_or_a_concurrent_publication() {
        let fixture = Fixture::new();
        let destination = fixture.destination();
        let target = fixture.target(destination.to_str().unwrap()).unwrap();
        let concurrent = fixture.target(destination.to_str().unwrap()).unwrap();
        let (mut stage, successor) = candidate(&target);
        let mut recovery = Recovery::default();
        flock(
            &concurrent.original.file,
            FlockOperation::NonBlockingLockExclusive,
        )
        .unwrap();
        assert!(
            target
                .commit_checked(
                    &mut stage,
                    &successor,
                    false,
                    &mut recovery,
                    || Ok(()),
                    |_| unreachable!()
                )
                .is_err()
        );
        assert!(!recovery.uncertain);
        assert_eq!(fs::read(&destination).unwrap(), OLD);
        drop(concurrent);

        let ancestor = Directory::open_parent(fixture.0.to_str().unwrap()).unwrap();
        flock(&ancestor.fd, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert!(
            target
                .commit_checked(
                    &mut stage,
                    &successor,
                    false,
                    &mut recovery,
                    || Ok(()),
                    |_| unreachable!()
                )
                .is_err()
        );
        assert!(!recovery.uncertain);
        assert_eq!(fs::read(&destination).unwrap(), OLD);
        drop(ancestor);

        let result = target.commit_checked(
            &mut stage,
            &successor,
            false,
            &mut recovery,
            || Ok(()),
            |_| {
                fs::rename(fixture.0.join("receiver"), fixture.0.join("moved"))?;
                fs::create_dir(fixture.0.join("receiver"))?;
                fs::write(&destination, b"external file")?;
                Ok(true)
            },
        );
        assert!(result.is_err());
        assert!(recovery.uncertain);
        assert!(recovery.path.is_none());
        assert_eq!(fs::read(&destination).unwrap(), b"external file");
        assert_eq!(
            fs::read(fixture.0.join("moved").join(&stage.name).join(ORIGINAL)).unwrap(),
            OLD
        );
    }
}
