//! Guarded publication of an individually reviewed privileged rsync receiver.

use std::io::Seek as _;
use std::os::fd::AsRawFd as _;

use kortty_server_archive::rsync_tree::{ExistingTree, MAX_BYTES};
use rustix::fs::{FlockOperation, flock};

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
    original: CheckedFile,
}

pub(in crate::server) struct CheckedFile {
    file: File,
    stamp: Stamp,
    inventory: Inventory,
    attributes: BTreeMap<String, Vec<u8>>,
}

impl<'a> Target<'a> {
    pub(in crate::server) fn open_at(destination: &'a str, parent: Directory) -> io::Result<Self> {
        let (path, name) = absolute_file(destination)?;
        if parent.stat.st_uid != geteuid().as_raw() || parent.stat.st_mode & 0o7022 != 0 {
            return Err(changed());
        }
        let mount = mount_id(&parent.fd)?;
        let parent_attributes = read_xattrs(&parent.fd)?;
        let original = CheckedFile::open(&parent, name, mount)?;
        let target = Self {
            destination,
            path,
            name,
            parent,
            parent_attributes,
            mount,
            original,
        };
        target.require_current()?;
        Ok(target)
    }

    pub(in crate::server) fn review(&self) -> ExistingTree {
        self.proof(&self.original, &self.original.stamp)
    }

    pub(in crate::server) fn parent(&self) -> &Directory {
        &self.parent
    }

    pub(in crate::server) fn destination(&self) -> &str {
        self.destination
    }

    fn proof(&self, file: &CheckedFile, stamp: &Stamp) -> ExistingTree {
        let mut hash = Sha256::new_with_prefix(b"kortty-rsync-existing-file-v1\0");
        hash.update((self.destination.len() as u32).to_be_bytes());
        hash.update(self.destination.as_bytes());
        hash.update(self.parent.stat.st_dev.to_be_bytes());
        hash.update(self.parent.stat.st_ino.to_be_bytes());
        hash.update(self.parent.stat.st_mode.to_be_bytes());
        hash.update(self.parent.stat.st_uid.to_be_bytes());
        hash.update(self.parent.stat.st_gid.to_be_bytes());
        hash.update(self.mount.to_be_bytes());
        hash_attributes(&mut hash, &self.parent_attributes);
        stamp.hash_into(&mut hash);
        hash.update(file.inventory.digest);
        hash_attributes(&mut hash, &file.attributes);
        ExistingTree {
            inventory: file.inventory,
            fingerprint: hash.finalize().into(),
        }
    }

    pub(in crate::server) fn prepare_successor(
        &self,
        stage: &UploadStage<'_>,
        expected: Inventory,
    ) -> io::Result<CheckedFile> {
        self.require_current()?;
        self.verify_stage(stage)?;
        let received = CheckedFile::open(&stage.payload, FILE, self.mount)?;
        if received.inventory != expected || received.stamp.mode & 0o7777 != 0o600 {
            return Err(invalid(
                "The received rsync file differs from the reviewed input",
            ));
        }
        copy_metadata(&self.original.file, &received.file)?;
        let successor = CheckedFile::inspect(received.file)?;
        successor.require_at(&stage.payload, FILE, false)?;
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
        successor.require_at(&stage.payload, FILE, false)?;
        if recurring {
            flock(&successor.file, FlockOperation::NonBlockingLockExclusive)?;
        }
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
            self.original.require_at(&stage.directory, ORIGINAL, true)?;
            successor.require_at(&stage.payload, FILE, false)?;
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

    fn require_current(&self) -> io::Result<()> {
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
        let current = Directory::open_parent(self.path)?;
        if !same_node(&self.parent.stat, &current.stat)
            || current.stat.st_uid != self.parent.stat.st_uid
            || current.stat.st_gid != self.parent.stat.st_gid
            || current.stat.st_mode != self.parent.stat.st_mode
            || mount_id(&current.fd)? != self.mount
            || read_xattrs(&current.fd)? != self.parent_attributes
        {
            return Err(changed());
        }
        Ok(())
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

impl CheckedFile {
    fn open(parent: &Directory, name: &str, mount: u64) -> io::Result<Self> {
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
            || mount_id(&held)? != mount
        {
            return Err(changed());
        }
        let file = File::from(open(
            format!("/proc/self/fd/{}", held.as_raw_fd()),
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let checked = Self::inspect(file)?;
        if checked.stamp != original {
            return Err(changed());
        }
        checked.require_at(parent, name, false)?;
        Ok(checked)
    }

    fn inspect(file: File) -> io::Result<Self> {
        let stamp = Stamp::of(&file.metadata()?);
        let attributes = read_xattrs(&file)?;
        let inventory = file_inventory(&file, stamp.size)?;
        if !stamp.matches(&file, false)? {
            return Err(changed());
        }
        Ok(Self {
            file,
            stamp,
            inventory,
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
}

fn file_inventory(mut file: &File, bytes: u64) -> io::Result<Inventory> {
    if bytes > MAX_BYTES {
        return Err(invalid(
            "The privileged rsync receiver exceeds the file size limit",
        ));
    }
    file.rewind()?;
    let mut manifest = Manifest::default();
    manifest.add("", Some((bytes, hash(file, bytes)?)))?;
    Ok(manifest.finish())
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
    invalid("The reviewed privileged rsync file changed or is unavailable")
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
