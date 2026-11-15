//! Review a protected Linux source and copy it into a sealed handoff for the ordinary SSH login.

use std::io::Write;
use std::os::fd::AsRawFd as _;

use kortty_server_archive::privileged_rsync::{
    Decision, Operation, Reply, Review, SourceLimits, Spec, read_decision, write_reply,
};
use kortty_server_archive::rsync_tree::projection::Entry;
use kortty_server_archive::rsync_tree::{
    Inventory, MAX_BYTES, MAX_DEPTH, MAX_ENTRIES, Manifest, hash, valid_name,
};
use rustix::fs::{XattrFlags, fgetxattr, fsetxattr};

use super::super::{draft_replace::mount_id, privileged_archive::require_source_node, *};
use super::policy::{Rules, require_source_file};

const PAYLOAD: &str = "payload";
const ACL: &str = "system.posix_acl_access";
const READ: u16 = 4;
const SEARCH: u16 = 1;
const SOURCE_CHANGED: &str = "The reviewed privileged rsync source changed or is unavailable";
const STAGE_CHANGED: &str = "The private privileged rsync handoff changed or is unavailable";
const LIMIT_EXCEEDED: &str = "The privileged rsync tree exceeds its transfer limits";
const MAXIMUM: SourceLimits = SourceLimits {
    entries: MAX_ENTRIES,
    bytes: MAX_BYTES,
};

pub(super) fn review(rules: &Rules, spec: Spec) -> io::Result<Review> {
    checked_review(rules, spec, MAXIMUM, None)
}

pub(super) fn review_manifest(rules: &Rules, spec: Spec) -> io::Result<(Review, Vec<Entry>)> {
    review_limited(rules, spec, MAXIMUM)
}

pub(super) fn review_limited(
    rules: &Rules,
    spec: Spec,
    limits: SourceLimits,
) -> io::Result<(Review, Vec<Entry>)> {
    let mut entries = Vec::new();
    let review = checked_review(rules, spec, limits, Some(&mut entries))?;
    Ok((review, entries))
}

fn checked_review(
    rules: &Rules,
    spec: Spec,
    limits: SourceLimits,
    entries: Option<&mut Vec<Entry>>,
) -> io::Result<Review> {
    rules.require_current()?;
    let (handoff, _, _) = rules.handoff()?;
    let (inventory, fingerprint) = source_proof(rules, &spec, limits, None, entries)?;
    rules.require_current()?;
    let review = Review {
        spec,
        policy: rules.revision(),
        fingerprint,
        inventory: Some(inventory),
        handoff,
    };
    review
        .valid()
        .then_some(review)
        .ok_or_else(|| invalid(SOURCE_CHANGED))
}

pub(super) fn apply(
    rules: &Rules,
    review: Review,
    input: &mut impl Read,
    output: &mut impl Write,
) -> io::Result<Reply> {
    if !review.valid() || review.policy != rules.revision() {
        return Err(invalid(SOURCE_CHANGED));
    }
    let directory = is_directory(&review.spec)?;
    let expected = review.inventory.ok_or_else(|| invalid(SOURCE_CHANGED))?;
    let limits = inventory_limits(expected);
    rules.require_current()?;
    let (handoff, parent, mount) = rules.handoff()?;
    if handoff != review.handoff {
        return Err(invalid(STAGE_CHANGED));
    }
    let (uid, _) = rules.login();
    let mut stage = Stage::create(parent, mount, uid)?;
    let result = (|| {
        let current = source_proof(rules, &review.spec, limits, Some(stage.target()), None)?;
        if current != (expected, review.fingerprint) {
            return Err(invalid(SOURCE_CHANGED));
        }
        stage.verify(rules, &handoff, directory, expected)?;
        let path = format!("{handoff}/{}/{PAYLOAD}", stage.name);
        if !review.contains_payload(&path) {
            return Err(invalid(STAGE_CHANGED));
        }
        rules.require_current()?;
        stage.require_owned()?;
        // Every child is sealed before the wrapper becomes traversable by the login.
        seal(&stage.directory.fd, uid, SEARCH)?;
        fsync(&stage.directory.fd)?;
        rules.require_current()?;
        write_reply(&mut *output, &Reply::Prepared { path })?;
        match read_decision(input) {
            Ok(Decision::Abort) => Ok(Reply::Aborted),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(Reply::Aborted),
            Ok(Decision::Verify) => {
                stage.verify(rules, &handoff, directory, expected)?;
                require_acl(&stage.directory.fd, uid, SEARCH)?;
                if source_proof(rules, &review.spec, limits, None, None)? != current {
                    return Err(invalid(SOURCE_CHANGED));
                }
                rules.require_current()?;
                Ok(Reply::Finished {
                    inventory: expected,
                })
            }
            Ok(_) => Err(invalid(
                "The privileged rsync download expected Verify or Abort",
            )),
            Err(error) => Err(error),
        }
    })();
    stage.cleanup()?;
    result
}

fn is_directory(spec: &Spec) -> io::Result<bool> {
    match spec.operation {
        Operation::Download { directory } if spec.valid() => Ok(directory),
        _ => Err(invalid(SOURCE_CHANGED)),
    }
}

fn source_proof(
    rules: &Rules,
    spec: &Spec,
    limits: SourceLimits,
    target: Option<Target<'_>>,
    entries: Option<&mut Vec<Entry>>,
) -> io::Result<(Inventory, [u8; 32])> {
    let directory = is_directory(spec)?;
    let (prefix, name) = absolute_file(&spec.path)?;
    let (parent, mount) = rules.source_parent(&spec.path, prefix)?;
    let mut walk = Walk::new(mount, Access::Source, limits, entries);
    walk.fingerprint.update(spec.path.as_bytes());
    walk.fingerprint.update(mount.to_le_bytes());
    walk.fingerprint.update(parent.stat.st_dev.to_le_bytes());
    walk.fingerprint.update(parent.stat.st_ino.to_le_bytes());
    walk.entry(&parent, name, "", 0, Some(directory), target)?;
    let (current, current_mount) = rules.source_parent(&spec.path, prefix)?;
    if !same_node(&parent.stat, &current.stat) || current_mount != mount {
        return Err(invalid(SOURCE_CHANGED));
    }
    Ok((walk.manifest.finish(), walk.fingerprint.finalize().into()))
}

fn inventory_limits(inventory: Inventory) -> SourceLimits {
    SourceLimits {
        entries: inventory.entries,
        bytes: inventory.bytes,
    }
}

#[derive(Clone, Copy)]
enum Access {
    Source,
    Handoff(u32),
}

struct Walk<'entries> {
    mount: u64,
    access: Access,
    manifest: Manifest,
    remaining: SourceLimits,
    fingerprint: Sha256,
    entries: Option<&'entries mut Vec<Entry>>,
}

impl<'entries> Walk<'entries> {
    fn new(
        mount: u64,
        access: Access,
        limits: SourceLimits,
        entries: Option<&'entries mut Vec<Entry>>,
    ) -> Self {
        Self {
            mount,
            access,
            manifest: Manifest::default(),
            remaining: limits,
            fingerprint: Sha256::new_with_prefix(b"kortty-privileged-rsync-source-v1\0"),
            entries,
        }
    }

    fn entry(
        &mut self,
        parent: &Directory,
        name: &str,
        relative: &str,
        depth: usize,
        root_kind: Option<bool>,
        target: Option<Target<'_>>,
    ) -> io::Result<()> {
        let named = statat(&parent.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        match FileType::from_raw_mode(named.st_mode) {
            FileType::Directory if root_kind != Some(false) => {
                let directory = parent.child(name)?;
                if !same_node(&named, &directory.stat) {
                    return Err(invalid(SOURCE_CHANGED));
                }
                match self.access {
                    Access::Source => require_source_node(&directory, true, self.mount)?,
                    Access::Handoff(uid) => {
                        require_owned(&directory.fd, self.mount, true)?;
                        require_acl(&directory.fd, uid, READ | SEARCH)?;
                    }
                }
                self.directory(&directory, relative, depth, target)?;
            }
            FileType::RegularFile if root_kind != Some(true) => {
                let file = open_regular(parent, name, &named)?;
                match self.access {
                    Access::Source => require_source_file(&file, self.mount)?,
                    Access::Handoff(uid) => {
                        require_owned(&file, self.mount, false)?;
                        require_acl(&file, uid, READ)?;
                    }
                }
                self.file(&file, relative, target)?;
            }
            _ => return Err(invalid(SOURCE_CHANGED)),
        }
        parent.require_child(name, &named)
    }

    fn directory(
        &mut self,
        directory: &Directory,
        relative: &str,
        depth: usize,
        target: Option<Target<'_>>,
    ) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("The privileged rsync tree exceeds its depth limit"));
        }
        let before = self.observe(&directory.fd, relative)?;
        if !relative.is_empty() {
            self.record(relative, None)?;
        }
        let copy = target.map(|target| target.directory()).transpose()?;
        for name in names(directory, self.remaining.entries as usize)? {
            let child = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            let target = copy.as_ref().map(|(directory, uid)| Target {
                parent: directory,
                name: &name,
                uid: *uid,
            });
            self.entry(directory, &name, &child, depth + 1, None, target)?;
        }
        if let Some((copy, uid)) = copy {
            seal(&copy.fd, uid, READ | SEARCH)?;
            fsync(&copy.fd)?;
        }
        unchanged(&directory.fd, &before)
    }

    fn file(&mut self, file: &File, relative: &str, target: Option<Target<'_>>) -> io::Result<()> {
        let before = self.observe(file, relative)?;
        self.require_capacity(before.size)?;
        let digest = if let Some(target) = target {
            let mut output = target.file()?;
            let digest = hash(
                CopyRead {
                    input: file,
                    output: &mut output,
                },
                before.size,
            )?;
            seal(&output, target.uid, READ)?;
            output.sync_all()?;
            digest
        } else {
            hash(file, before.size)?
        };
        unchanged(file, &before)?;
        self.record(relative, Some((before.size, digest)))
    }

    fn record(&mut self, relative: &str, file: Option<(u64, [u8; 32])>) -> io::Result<()> {
        let bytes = file.map_or(0, |(bytes, _)| bytes);
        self.require_capacity(bytes)?;
        self.manifest.add(relative, file)?;
        self.remaining.entries -= 1;
        self.remaining.bytes -= bytes;
        if let Some(entries) = &mut self.entries {
            entries.push(Entry {
                relative: relative.to_owned(),
                file,
            });
        }
        Ok(())
    }

    fn require_capacity(&self, bytes: u64) -> io::Result<()> {
        if self.remaining.entries == 0 || bytes > self.remaining.bytes {
            return Err(invalid(LIMIT_EXCEEDED));
        }
        Ok(())
    }

    fn observe(&mut self, file: &File, relative: &str) -> io::Result<Stamp> {
        let stamp = Stamp::of(&file.metadata()?);
        self.fingerprint
            .update((relative.len() as u32).to_le_bytes());
        self.fingerprint.update(relative.as_bytes());
        stamp.hash_into(&mut self.fingerprint);
        Ok(stamp)
    }
}

fn names(directory: &Directory, limit: usize) -> io::Result<Vec<String>> {
    let mut result = Vec::new();
    for entry in Dir::read_from(&directory.fd)? {
        let entry = entry?;
        let name = entry.file_name().to_str().map_err(io::Error::other)?;
        if matches!(name, "." | "..") {
            continue;
        }
        if result.len() == limit || !valid_name(name) {
            return Err(invalid(
                "The privileged rsync stage contains an unsafe name or too many entries",
            ));
        }
        result.push(name.to_owned());
    }
    result.sort_unstable();
    Ok(result)
}

fn open_regular(parent: &Directory, name: &str, named: &Stat) -> io::Result<File> {
    let pin = openat(
        &parent.fd,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let held = fstat(&pin)?;
    if !same_node(named, &held) || FileType::from_raw_mode(held.st_mode) != FileType::RegularFile {
        return Err(invalid(SOURCE_CHANGED));
    }
    let file = File::from(open(
        format!("/proc/self/fd/{}", pin.as_raw_fd()),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOCTTY,
        Mode::empty(),
    )?);
    if !same_node(&held, &fstat(&file)?) {
        return Err(invalid(SOURCE_CHANGED));
    }
    Ok(file)
}

fn unchanged(file: &File, before: &Stamp) -> io::Result<()> {
    (&Stamp::of(&file.metadata()?) == before)
        .then_some(())
        .ok_or_else(|| invalid(SOURCE_CHANGED))
}

struct CopyRead<'a> {
    input: &'a File,
    output: &'a mut File,
}

impl Read for CopyRead<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(buffer)?;
        self.output.write_all(&buffer[..count])?;
        Ok(count)
    }
}

struct Target<'a> {
    parent: &'a Directory,
    name: &'a str,
    uid: u32,
}

impl Target<'_> {
    fn directory(&self) -> io::Result<(Directory, u32)> {
        mkdirat(&self.parent.fd, self.name, PRIVATE_DIRECTORY)?;
        let directory = self.parent.child(self.name)?;
        Ok((directory, self.uid))
    }

    fn file(&self) -> io::Result<File> {
        Ok(File::from(openat(
            &self.parent.fd,
            self.name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            PRIVATE_FILE,
        )?))
    }
}

struct Stage {
    parent: Directory,
    name: String,
    directory: Directory,
    mount: u64,
    uid: u32,
    active: bool,
}

impl Stage {
    fn create(parent: Directory, mount: u64, uid: u32) -> io::Result<Self> {
        if uid == 0 || uid == u32::MAX {
            return Err(invalid(STAGE_CHANGED));
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let name = format!(".kortty-{}.kortty-part", hex::encode(random));
        mkdirat(&parent.fd, name.as_str(), PRIVATE_DIRECTORY)?;
        let directory = parent.child(&name)?;
        let stage = Self {
            parent,
            name,
            directory,
            mount,
            uid,
            active: true,
        };
        stage.require_owned()?;
        fchmod(&stage.directory.fd, PRIVATE_DIRECTORY)?;
        Ok(stage)
    }

    fn target(&self) -> Target<'_> {
        Target {
            parent: &self.directory,
            name: PAYLOAD,
            uid: self.uid,
        }
    }

    fn require_owned(&self) -> io::Result<()> {
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        require_owned(&self.directory.fd, self.mount, true)
    }

    fn verify(
        &self,
        rules: &Rules,
        handoff: &str,
        directory: bool,
        expected: Inventory,
    ) -> io::Result<()> {
        rules.require_current()?;
        let (prefix, parent, mount) = rules.handoff()?;
        if prefix != handoff || mount != self.mount || !same_node(&self.parent.stat, &parent.stat) {
            return Err(invalid(STAGE_CHANGED));
        }
        self.require_owned()?;
        if names(&self.directory, 1)? != [PAYLOAD] {
            return Err(invalid(STAGE_CHANGED));
        }
        let mut walk = Walk::new(
            self.mount,
            Access::Handoff(self.uid),
            inventory_limits(expected),
            None,
        );
        walk.entry(&self.directory, PAYLOAD, "", 0, Some(directory), None)?;
        if walk.manifest.finish() != expected {
            return Err(invalid(STAGE_CHANGED));
        }
        Ok(())
    }

    fn cleanup(&mut self) -> io::Result<()> {
        self.require_owned()?;
        fchmod(&self.directory.fd, PRIVATE_DIRECTORY)?;
        fsync(&self.directory.fd)?;
        discard(
            &self.directory,
            self.mount,
            0,
            &mut (MAX_ENTRIES as usize + 1),
        )?;
        self.require_owned()?;
        unlinkat(&self.parent.fd, self.name.as_str(), AtFlags::REMOVEDIR)?;
        self.active = false;
        fsync(&self.parent.fd)?;
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.active {
            let _ = self.cleanup();
        }
    }
}

fn require_owned(file: &File, mount: u64, directory: bool) -> io::Result<()> {
    let stat = fstat(file)?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    if stat.st_uid != 0
        || stat.st_mode & 0o7022 != 0
        || mount_id(file)? != mount
        || if directory {
            kind != FileType::Directory
        } else {
            kind != FileType::RegularFile || stat.st_nlink != 1
        }
    {
        return Err(invalid(STAGE_CHANGED));
    }
    Ok(())
}

fn discard(
    directory: &Directory,
    mount: u64,
    depth: usize,
    remaining: &mut usize,
) -> io::Result<()> {
    if depth > MAX_DEPTH + 1 {
        return Err(invalid(STAGE_CHANGED));
    }
    require_owned(&directory.fd, mount, true)?;
    for name in names(directory, *remaining)? {
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| invalid(STAGE_CHANGED))?;
        let named = statat(&directory.fd, &name, AtFlags::SYMLINK_NOFOLLOW)?;
        let flags = match FileType::from_raw_mode(named.st_mode) {
            FileType::Directory => {
                let child = directory.child(&name)?;
                if !same_node(&named, &child.stat) {
                    return Err(invalid(STAGE_CHANGED));
                }
                discard(&child, mount, depth + 1, remaining)?;
                AtFlags::REMOVEDIR
            }
            FileType::RegularFile => {
                let file = open_regular(directory, &name, &named)?;
                require_owned(&file, mount, false)?;
                AtFlags::empty()
            }
            _ => return Err(invalid(STAGE_CHANGED)),
        };
        directory.require_child(&name, &named)?;
        unlinkat(&directory.fd, &name, flags)?;
    }
    Ok(())
}

fn acl_bytes(uid: u32, access: u16, owner: u16) -> [u8; 44] {
    // Linux's POSIX ACL xattr has a little-endian version and five fixed-width entries.
    let mut bytes = [0_u8; 44];
    bytes[..4].copy_from_slice(&2_u32.to_le_bytes());
    for (slot, (tag, permissions, id)) in bytes[4..].as_chunks_mut::<8>().0.iter_mut().zip([
        (1_u16, owner, u32::MAX),
        (2, access, uid),
        (4, 0, u32::MAX),
        (16, access, u32::MAX),
        (32, 0, u32::MAX),
    ]) {
        slot[..2].copy_from_slice(&tag.to_le_bytes());
        slot[2..4].copy_from_slice(&permissions.to_le_bytes());
        slot[4..].copy_from_slice(&id.to_le_bytes());
    }
    bytes
}

fn seal(file: &File, uid: u32, access: u16) -> io::Result<()> {
    let owner = if file.metadata()?.is_dir() { 7 } else { 6 };
    fsetxattr(
        file,
        ACL,
        &acl_bytes(uid, access, owner),
        XattrFlags::empty(),
    )?;
    require_acl(file, uid, access)
}

fn require_acl(file: &File, uid: u32, access: u16) -> io::Result<()> {
    let metadata = file.metadata()?;
    let owner = if metadata.is_dir() { 7 } else { 6 };
    let expected_mode = (u32::from(owner) << 6) | (u32::from(access) << 3);
    let mut observed = [0_u8; 44];
    if metadata.mode() & 0o7777 != expected_mode
        || fgetxattr(file, ACL, &mut observed[..])? != observed.len()
        || observed != acl_bytes(uid, access, owner)
    {
        return Err(invalid(STAGE_CHANGED));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;

    use rustix::fs::mkfifoat;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).unwrap();
            let scratch = Self(
                std::env::temp_dir()
                    .canonicalize()
                    .unwrap()
                    .join(format!("kortty-rsync-source-{}", hex::encode(random))),
            );
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&scratch.0)
                .unwrap();
            scratch
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn privileged_download_never_opens_a_substituted_fifo_for_contents() {
        let scratch = Scratch::new();
        let parent = Directory::open_parent(scratch.0.to_str().unwrap()).unwrap();
        let source = "source";
        let original = File::from(
            openat(
                &parent.fd,
                source,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
                PRIVATE_FILE,
            )
            .unwrap(),
        );
        let expected = fstat(&original).unwrap();
        assert!(open_regular(&parent, source, &expected).is_ok());
        unlinkat(&parent.fd, source, AtFlags::empty()).unwrap();
        mkfifoat(&parent.fd, source, PRIVATE_FILE).unwrap();
        assert!(open_regular(&parent, source, &expected).is_err());
        let fifo = statat(&parent.fd, source, AtFlags::SYMLINK_NOFOLLOW).unwrap();
        assert!(open_regular(&parent, source, &fifo).is_err());
    }

    #[test]
    fn privileged_download_checks_remaining_source_limits_before_hashing() {
        let scratch = Scratch::new();
        let path = scratch.0.join("file");
        let mut unreadable = File::create(&path).unwrap();
        unreadable.write_all(b"1234").unwrap();
        let mut walk = Walk::new(
            0,
            Access::Source,
            SourceLimits {
                entries: 2,
                bytes: 7,
            },
            None,
        );
        walk.file(&File::open(&path).unwrap(), "a", None).unwrap();
        assert_eq!(
            walk.file(&unreadable, "b", None).unwrap_err().to_string(),
            LIMIT_EXCEEDED
        );
        walk.record("b", None).unwrap();
        assert_eq!(
            walk.file(&unreadable, "c", None).unwrap_err().to_string(),
            LIMIT_EXCEEDED
        );
        let inventory = walk.manifest.finish();
        assert_eq!(
            (inventory.entries, inventory.files, inventory.bytes),
            (2, 1, 4)
        );
    }

    #[test]
    fn privileged_download_zero_remaining_entries_allows_only_an_empty_source_root() {
        let scratch = Scratch::new();
        let parent = Directory::open_parent(scratch.0.to_str().unwrap()).unwrap();
        let limits = SourceLimits {
            entries: 0,
            bytes: 0,
        };
        let mut walk = Walk::new(0, Access::Source, limits, None);
        walk.directory(&parent, "", 0, None).unwrap();
        assert_eq!(walk.manifest.finish().entries, 0);
        File::create(scratch.0.join("empty")).unwrap();
        let mut walk = Walk::new(0, Access::Source, limits, None);
        assert!(walk.directory(&parent, "", 0, None).is_err());
    }

    #[test]
    fn privileged_handoff_acl_grants_only_the_named_login() {
        let bytes = acl_bytes(60_001, READ | SEARCH, 7);
        assert_eq!(&bytes[..4], &2_u32.to_le_bytes());
        let entries: Vec<_> = bytes[4..]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|entry| {
                (
                    u16::from_le_bytes(entry[..2].try_into().unwrap()),
                    u16::from_le_bytes(entry[2..4].try_into().unwrap()),
                    u32::from_le_bytes(entry[4..].try_into().unwrap()),
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                (1, 7, u32::MAX),
                (2, 5, 60_001),
                (4, 0, u32::MAX),
                (16, 5, u32::MAX),
                (32, 0, u32::MAX),
            ]
        );
    }

    #[test]
    fn privileged_download_manifest_records_the_complete_bounded_scan_order() {
        let mut entries = Vec::new();
        let mut walk = Walk::new(0, Access::Source, MAXIMUM, Some(&mut entries));
        let nested_file = Some((3, [1; 32]));
        let sibling_file = Some((1, [2; 32]));
        for (relative, file) in [
            ("a", None),
            ("a/nested", nested_file),
            ("a-sibling", sibling_file),
        ] {
            walk.record(relative, file).unwrap();
        }
        for index in 3..MAX_ENTRIES {
            walk.record(&format!("b{index:04}"), None).unwrap();
        }
        let expected = walk.manifest.finish();
        assert_eq!(entries.len(), MAX_ENTRIES as usize);
        assert_eq!(entries[0].relative, "a");
        assert_eq!(entries[1].relative, "a/nested");
        assert_eq!(entries[1].file, nested_file);
        assert_eq!(entries[2].relative, "a-sibling");

        let mut manifest = Manifest::default();
        for entry in &entries {
            manifest.add(&entry.relative, entry.file).unwrap();
        }
        assert_eq!(manifest.finish(), expected);
        let mut walk = Walk::new(0, Access::Source, MAXIMUM, Some(&mut entries));
        assert!(walk.record(&"x".repeat(4097), None).is_err());
        assert_eq!(entries.len(), MAX_ENTRIES as usize);
    }
}
