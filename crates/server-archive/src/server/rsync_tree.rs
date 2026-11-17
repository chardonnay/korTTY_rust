use kortty_server_archive::rsync_tree::{
    Decision, Inventory, MAX_DEPTH, MAX_ENTRIES, Manifest, Prepare, Reply, hash, read_frame,
    valid_name, write_frame,
};

use super::*;

const PAYLOAD: &str = "payload";
#[cfg(target_os = "linux")]
const REPLACED_NAME: &str = "replaced-name";

#[cfg(target_os = "linux")]
mod existing;
#[cfg(target_os = "linux")]
mod existing_file;
#[cfg(target_os = "linux")]
pub(super) use existing::{
    FirstTree as CheckedTree, Published as ExistingPublication, Recovery as ExistingRecovery,
    Target as ExistingTarget, VacantTarget as VacantTree,
};
#[cfg(target_os = "linux")]
pub(crate) use existing::{
    serve as serve_existing, serve_v2 as serve_existing_v2, serve_v3 as serve_existing_v3,
};
#[cfg(target_os = "linux")]
pub(super) use existing_file::{
    CheckedFile, FILE as RECEIVED_FILE, Target as ExistingFile, VacantTarget as VacantFile,
    directory_fingerprint,
};

#[cfg(target_os = "linux")]
fn lock_receiver_ancestors(path: &str, expected: &Directory) -> io::Result<Vec<Directory>> {
    use rustix::fs::{FlockOperation, flock};

    let mut held = vec![Directory::open_parent("/")?];
    for name in path[1..].split('/').filter(|name| !name.is_empty()) {
        let anchor = held.last().expect("held root directory");
        anchor.require_protected(true)?;
        let directory = anchor.child(name)?;
        match flock(&directory.fd, FlockOperation::NonBlockingLockShared) {
            Ok(()) => held.push(directory),
            Err(Errno::WOULDBLOCK) => {
                return Err(invalid(
                    "Another rsync publication holds an ancestor receiver",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    }
    let parent = held.last().expect("held receiver parent");
    parent.require_protected(false)?;
    if !same_node(&parent.stat, &expected.stat)
        || super::draft_replace::mount_id(&parent.fd)?
            != super::draft_replace::mount_id(&expected.fd)?
    {
        return Err(invalid("The reviewed rsync receiver parent changed"));
    }
    Ok(held)
}

pub(crate) fn serve(mut input: impl Read, mut output: impl io::Write) -> io::Result<()> {
    let result = (|| -> Result<Reply, Failure> {
        if getuid() != geteuid() || getgid() != getegid() {
            return Err(invalid("Rsync tree upload requires an ordinary account").into());
        }
        umask(Mode::from_bits_truncate(0o077));
        let Prepare {
            destination,
            expected,
        } = read_frame(&mut input)?;
        if !expected.valid() {
            return Err(invalid("The reviewed rsync inventory is invalid").into());
        }
        let (path, name) = absolute_file(&destination)?;
        let parent = Directory::open_parent(path)?;
        require_absent(&parent.fd, name)?;
        let mut stage = UploadStage::create(&parent)?;
        write_frame(
            &mut output,
            &Reply::Prepared {
                stage: stage.name.clone(),
            },
        )?;
        let operation = match read_frame(&mut input) {
            Ok(Decision::Verify) => (|| {
                let witness = stage.verify_inventory(path, name, expected)?;
                write_frame(&mut output, &Reply::Verified)?;
                match read_frame(&mut input)? {
                    Decision::Commit => {
                        stage.publish(path, name, &witness, || Ok(()))?;
                        Ok(Reply::Published {
                            destination: destination.clone(),
                            inventory: expected,
                        })
                    }
                    Decision::Abort => Ok(Reply::Aborted),
                    Decision::Verify => Err(invalid("Unexpected rsync helper decision")),
                }
            })(),
            Ok(Decision::Abort) => Ok(Reply::Aborted),
            Ok(Decision::Commit) => Err(invalid("The rsync stage was not verified")),
            Err(error) => Err(error),
        };
        match operation {
            Ok(reply) => {
                stage.discard().map_err(|error| stage.failure(error))?;
                Ok(reply)
            }
            Err(error) => Err(stage.failure(error)),
        }
    })();
    let reply = result.unwrap_or_else(|failure| Reply::Failed {
        message: failure.message,
        outcome_unknown: failure.outcome_unknown,
    });
    write_frame(output, &reply)
}

pub(super) struct UploadStage<'a> {
    parent: &'a Directory,
    name: String,
    directory: Directory,
    pub(super) payload: Directory,
    mount: Option<u64>,
    owned: bool,
    pub(super) published: bool,
    #[cfg(target_os = "linux")]
    replaced_name: Option<Stat>,
}

impl<'a> UploadStage<'a> {
    pub(super) fn create(parent: &'a Directory) -> io::Result<Self> {
        Self::create_on(parent, None, None)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn create_checked(parent: &'a Directory) -> io::Result<Self> {
        Self::create_on(
            parent,
            Some(super::draft_replace::mount_id(&parent.fd)?),
            None,
        )
    }

    #[cfg(target_os = "linux")]
    pub(super) fn create_for_transfer(
        parent: &'a Directory,
        prefix: [u8; 8],
        original_name: &str,
    ) -> io::Result<Self> {
        use io::Write as _;

        let mut stage = Self::create_on(
            parent,
            Some(super::draft_replace::mount_id(&parent.fd)?),
            Some(prefix),
        )?;
        let mut file = File::from(openat(
            &stage.directory.fd,
            REPLACED_NAME,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            PRIVATE_FILE,
        )?);
        stage.replaced_name = Some(fstat(&file)?);
        fchmod(&file, PRIVATE_FILE)?;
        file.write_all(original_name.as_bytes())?;
        file.sync_all()?;
        fsync(&stage.directory.fd)?;
        Ok(stage)
    }

    fn create_on(
        parent: &'a Directory,
        mount: Option<u64>,
        prefix: Option<[u8; 8]>,
    ) -> io::Result<Self> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(io::Error::other)?;
        if let Some(prefix) = prefix {
            nonce[..prefix.len()].copy_from_slice(&prefix);
        }
        let name = format!(".kortty-{}.kortty-part", hex::encode(nonce));
        mkdirat(&parent.fd, name.as_str(), PRIVATE_DIRECTORY)?;
        let directory = parent.child(&name)?;
        let owned = |name: &str, parent: &Directory| {
            let child = parent.child(name)?;
            parent.require_child(name, &child.stat)?;
            require_stage_mount(&child.fd, mount)?;
            require_private(&child.fd, 0o700)?;
            Ok::<_, io::Error>(child)
        };
        let result = (|| {
            require_stage_mount(&directory.fd, mount)?;
            require_private(&directory.fd, 0o700)?;
            parent.require_child(&name, &directory.stat)?;
            // Confirm the filesystem supports atomic no-replace before receiving.
            match renameat_with(
                &parent.fd,
                name.as_str(),
                &parent.fd,
                name.as_str(),
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            mkdirat(&directory.fd, PAYLOAD, PRIVATE_DIRECTORY)?;
            owned(PAYLOAD, &directory)
        })();
        match result {
            Ok(payload) => Ok(Self {
                parent,
                name,
                directory,
                payload,
                mount,
                owned: true,
                published: false,
                #[cfg(target_os = "linux")]
                replaced_name: None,
            }),
            Err(error) => {
                if parent.require_child(&name, &directory.stat).is_ok()
                    && require_stage_mount(&directory.fd, mount).is_ok()
                {
                    let _ = unlinkat(&directory.fd, PAYLOAD, AtFlags::REMOVEDIR);
                    let _ = unlinkat(&parent.fd, name.as_str(), AtFlags::REMOVEDIR);
                }
                Err(error)
            }
        }
    }

    fn verify(&self, path: &str) -> io::Result<()> {
        let current = Directory::open_parent(path)?;
        if !same_node(&current.stat, &self.parent.stat)
            || current.stat.st_mode != self.parent.stat.st_mode
            || current.stat.st_uid != self.parent.stat.st_uid
            || current.stat.st_gid != self.parent.stat.st_gid
        {
            return Err(invalid("The server rsync destination parent changed"));
        }
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        self.directory.require_child(PAYLOAD, &self.payload.stat)?;
        require_stage_mount(&current.fd, self.mount)?;
        require_stage_mount(&self.directory.fd, self.mount)?;
        require_stage_mount(&self.payload.fd, self.mount)?;
        require_private(&self.directory.fd, 0o700)?;
        require_private(&self.payload.fd, 0o700)
    }

    fn verify_inventory(&self, path: &str, name: &str, expected: Inventory) -> io::Result<Witness> {
        self.verify(path)?;
        require_absent(&self.parent.fd, name)?;
        self.verify_received(expected)
    }

    pub(super) fn verify_received(&self, expected: Inventory) -> io::Result<Witness> {
        self.verify_received_with(expected, Manifest::default())
    }

    fn verify_received_with(
        &self,
        expected: Inventory,
        mut manifest: Manifest,
    ) -> io::Result<Witness> {
        let witness = scan(
            &self.payload,
            "",
            0,
            &mut manifest,
            Scan::Received { mount: self.mount },
        )?;
        if manifest.finish() != expected {
            return Err(invalid(
                "The received rsync tree differs from the reviewed inventory",
            ));
        }
        Ok(witness)
    }

    pub(super) fn publish(
        &mut self,
        path: &str,
        name: &str,
        witness: &Witness,
        permit: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_received(witness)?;
        self.verify(path)?;
        permit()?;
        match renameat_with(
            &self.directory.fd,
            PAYLOAD,
            &self.parent.fd,
            name,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => self.published = true,
            Err(Errno::EXIST | Errno::NOTEMPTY) => {
                return Err(invalid("The reviewed server destination already exists"));
            }
            Err(error) => {
                self.published = self
                    .directory
                    .require_child(PAYLOAD, &self.payload.stat)
                    .is_err();
                return Err(error.into());
            }
        }
        self.parent.require_child(name, &self.payload.stat)?;
        fsync(&self.directory.fd)?;
        fsync(&self.parent.fd)?;
        Ok(())
    }

    pub(super) fn discard(&mut self) -> io::Result<()> {
        if !self.owned {
            return Ok(());
        }
        self.owned = false;
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        require_stage_mount(&self.directory.fd, self.mount)?;
        if !self.published {
            self.directory.require_child(PAYLOAD, &self.payload.stat)?;
            let mut limit = MAX_ENTRIES as usize + 2;
            discard(
                &self.payload,
                self.payload.fd.metadata()?.dev(),
                0,
                &mut limit,
                self.mount,
            )?;
            self.directory.require_child(PAYLOAD, &self.payload.stat)?;
            unlinkat(&self.directory.fd, PAYLOAD, AtFlags::REMOVEDIR)?;
        }
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        #[cfg(target_os = "linux")]
        if let Some(note) = &self.replaced_name {
            self.directory.require_child(REPLACED_NAME, note)?;
            unlinkat(&self.directory.fd, REPLACED_NAME, AtFlags::empty())?;
        }
        unlinkat(&self.parent.fd, self.name.as_str(), AtFlags::REMOVEDIR)?;
        Ok(())
    }

    fn require_received(&self, witness: &Witness) -> io::Result<()> {
        witness.require_tree(&self.payload, Scan::Received { mount: self.mount }, false)
    }

    fn failure(&mut self, error: io::Error) -> Failure {
        let mut failure = Failure::from(error);
        failure.outcome_unknown = self.published;
        if self.discard().is_err() {
            failure
                .message
                .push_str(". A private rsync staging folder may remain");
        }
        failure
    }
}

impl Drop for UploadStage<'_> {
    fn drop(&mut self) {
        let _ = self.discard();
    }
}

fn require_private(file: &File, mode: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    #[cfg(target_os = "macos")]
    kortty_files::require_no_extended_acl(file)?;
    if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o7777 != mode {
        return Err(invalid("The private rsync permissions changed"));
    }
    Ok(())
}

pub(super) struct Witness {
    stamp: Stamp,
    children: Vec<(String, Node)>,
    #[cfg(target_os = "linux")]
    attributes: Option<[u8; 32]>,
}

enum Node {
    Directory(Witness),
    File {
        stamp: Stamp,
        // The existing-target transaction binds each retained or incoming file.
        #[cfg(target_os = "linux")]
        digest: [u8; 32],
    },
}

impl Witness {
    fn require_tree(&self, directory: &Directory, scan: Scan, moved: bool) -> io::Result<()> {
        scan.require_mount(&directory.fd)?;
        if !self.stamp.matches(&directory.fd, moved)? {
            return Err(invalid(
                "The received rsync directory changed before publication",
            ));
        }
        #[cfg(target_os = "linux")]
        if moved && self.attributes != scan.directory_attributes(&directory.fd)? {
            return Err(invalid("The renamed rsync directory attributes changed"));
        }
        #[cfg(target_os = "linux")]
        if matches!(scan, Scan::Existing { .. }) {
            self.require_names(directory)?;
        }
        for (name, node) in &self.children {
            match node {
                Node::Directory(child) => {
                    child.require_tree(&directory.child(name)?, scan, false)?;
                }
                Node::File {
                    stamp,
                    #[cfg(target_os = "linux")]
                    digest,
                } => {
                    let file =
                        File::from(openat(&directory.fd, name.as_str(), SOURCE, Mode::empty())?);
                    scan.require_mount(&file)?;
                    if !stamp.matches(&file, false)? {
                        return Err(invalid(
                            "The received rsync file changed before publication",
                        ));
                    }
                    #[cfg(target_os = "linux")]
                    if matches!(scan, Scan::Existing { .. })
                        && (hash(&file, stamp.size)? != *digest || !stamp.matches(&file, false)?)
                    {
                        return Err(invalid("The reviewed rsync file bytes changed"));
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        if matches!(scan, Scan::Existing { .. }) {
            self.require_names(directory)?;
        }
        if !self.stamp.matches(&directory.fd, moved)? {
            return Err(invalid(
                "The received rsync directory changed before publication",
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn require_names(&self, directory: &Directory) -> io::Result<()> {
        let names = names(directory, self.children.len())?;
        if !names.iter().eq(self.children.iter().map(|(name, _)| name)) {
            return Err(invalid("The reviewed rsync directory entries changed"));
        }
        Ok(())
    }
}

impl Stamp {
    fn matches(&self, file: &File, moved: bool) -> io::Result<bool> {
        let mut current = Stamp::of(&file.metadata()?);
        // Renames update the moved directory's ctime. Descendants stay exact.
        if moved {
            current.ctime = self.ctime;
            current.ctime_ns = self.ctime_ns;
        }
        Ok(current == *self)
    }
}

#[derive(Clone, Copy)]
enum Scan {
    Received {
        mount: Option<u64>,
    },
    #[cfg(target_os = "linux")]
    Existing {
        mount: u64,
    },
}

impl Scan {
    fn require_mount(self, file: &File) -> io::Result<()> {
        let mount = match self {
            Self::Received { mount } => mount,
            #[cfg(target_os = "linux")]
            Self::Existing { mount } => Some(mount),
        };
        require_stage_mount(file, mount)
    }

    fn require_entry(self, file: &File, mode: u32) -> io::Result<()> {
        self.require_mount(file)?;
        match self {
            Self::Received { .. } => require_private(file, mode),
            #[cfg(target_os = "linux")]
            Self::Existing { .. } => {
                let metadata = file.metadata()?;
                if metadata.uid() != geteuid().as_raw()
                    || metadata.mode() & 0o7022 != 0
                    || metadata.is_dir() && metadata.mode() & 0o700 != 0o700
                {
                    return Err(invalid(
                        "The rsync destination contains an unprotected entry",
                    ));
                }
                if metadata.is_file() {
                    super::draft_replace::read_xattrs(file)?;
                }
                Ok(())
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn directory_attributes(self, file: &File) -> io::Result<Option<[u8; 32]>> {
        if matches!(self, Self::Received { .. }) {
            return Ok(None);
        }
        let mut digest = Sha256::new();
        for (name, value) in super::draft_replace::read_xattrs(file)? {
            digest.update((name.len() as u32).to_be_bytes());
            digest.update(name.as_bytes());
            digest.update((value.len() as u32).to_be_bytes());
            digest.update(value);
        }
        Ok(Some(digest.finalize().into()))
    }
}

fn require_stage_mount(_file: &File, _mount: Option<u64>) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    if let Some(mount) = _mount
        && super::draft_replace::mount_id(_file)? != mount
    {
        return Err(invalid("The reviewed rsync tree crosses a mount boundary"));
    }
    Ok(())
}

fn scan(
    directory: &Directory,
    prefix: &str,
    depth: usize,
    manifest: &mut Manifest,
    purpose: Scan,
) -> io::Result<Witness> {
    if depth > MAX_DEPTH {
        return Err(invalid("Rsync directory exceeds its depth limit"));
    }
    let before = Stamp::of(&directory.fd.metadata()?);
    purpose.require_entry(&directory.fd, 0o700)?;
    #[cfg(target_os = "linux")]
    let attributes = purpose.directory_attributes(&directory.fd)?;
    let names = names(directory, manifest.remaining_entries())?;
    let mut children = Vec::with_capacity(names.len());
    for name in names {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let info = statat(&directory.fd, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
        if info.st_dev != directory.stat.st_dev {
            return Err(invalid("An rsync entry is on another filesystem"));
        }
        let node = match FileType::from_raw_mode(info.st_mode) {
            FileType::Directory => {
                let child = directory.child(&name)?;
                directory.require_child(&name, &child.stat)?;
                if !same_node(&child.stat, &info) {
                    return Err(invalid("The received rsync directory changed"));
                }
                manifest.add(&path, None)?;
                let child_witness = scan(&child, &path, depth + 1, manifest, purpose)?;
                directory.require_child(&name, &child.stat)?;
                Node::Directory(child_witness)
            }
            FileType::RegularFile => {
                let file = File::from(openat(&directory.fd, name.as_str(), SOURCE, Mode::empty())?);
                let held = fstat(&file)?;
                let size = u64::try_from(held.st_size).map_err(io::Error::other)?;
                if !same_node(&held, &info)
                    || held.st_nlink != 1
                    || size > manifest.remaining_bytes()
                {
                    return Err(invalid("The received rsync file is invalid"));
                }
                let before = Stamp::of(&file.metadata()?);
                purpose.require_entry(&file, 0o600)?;
                let digest = hash(&file, size)?;
                if Stamp::of(&file.metadata()?) != before {
                    return Err(invalid("The received rsync file changed"));
                }
                directory.require_child(&name, &held)?;
                if matches!(purpose, Scan::Received { .. }) {
                    fsync(&file)?;
                }
                manifest.add(&path, Some((size, digest)))?;
                Node::File {
                    stamp: before,
                    #[cfg(target_os = "linux")]
                    digest,
                }
            }
            _ => return Err(invalid("The rsync directory contains an unsupported entry")),
        };
        children.push((name, node));
    }
    if Stamp::of(&directory.fd.metadata()?) != before {
        return Err(invalid("The received rsync directory changed"));
    }
    if matches!(purpose, Scan::Received { .. }) {
        fsync(&directory.fd)?;
    }
    Ok(Witness {
        stamp: before,
        children,
        #[cfg(target_os = "linux")]
        attributes,
    })
}

fn names(directory: &Directory, limit: usize) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(&directory.fd)? {
        let entry = entry?;
        let name = entry.file_name().to_str().map_err(io::Error::other)?;
        if matches!(name, "." | "..") {
            continue;
        }
        if !valid_name(name) || names.len() == limit {
            return Err(invalid(
                "The rsync directory contains an unsafe name or too many entries",
            ));
        }
        names.push(name.to_owned());
    }
    names.sort_unstable();
    Ok(names)
}

fn discard(
    directory: &Directory,
    device: u64,
    depth: usize,
    limit: &mut usize,
    mount: Option<u64>,
) -> io::Result<()> {
    require_stage_mount(&directory.fd, mount)?;
    if depth > MAX_DEPTH {
        return Err(invalid("The private rsync stage exceeds its cleanup depth"));
    }
    for entry in Dir::read_from(&directory.fd)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        *limit = limit
            .checked_sub(1)
            .ok_or_else(|| invalid("The private rsync stage exceeds its cleanup limit"))?;
        let info = statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if info.st_dev != directory.stat.st_dev || info.st_uid != geteuid().as_raw() {
            return Err(invalid("The private rsync stage contains a foreign entry"));
        }
        let flags = if FileType::from_raw_mode(info.st_mode) == FileType::Directory {
            let child = Directory::from_fd(openat(&directory.fd, name, DIRECTORY, Mode::empty())?)?;
            if !same_node(&child.stat, &info) {
                return Err(invalid(
                    "The private rsync directory changed during cleanup",
                ));
            }
            if child.fd.metadata()?.dev() != device {
                return Err(invalid("The private rsync stage crosses a filesystem"));
            }
            discard(&child, device, depth + 1, limit, mount)?;
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        if !same_node(
            &statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW)?,
            &info,
        ) {
            return Err(invalid("The private rsync entry changed during cleanup"));
        }
        unlinkat(&directory.fd, name, flags)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::OpenOptionsExt as _;

    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt as _;

            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("kortty-rsync-test-{}", hex::encode(nonce)));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rsync_tree_never_replaces_a_late_empty_destination() {
        let root = Scratch::new();
        let path = root.0.to_str().unwrap();
        let parent = Directory::open_parent(path).unwrap();
        let mut stage = UploadStage::create(&parent).unwrap();
        let expected = Manifest::default().finish();
        let witness = stage.verify_inventory(path, "late", expected).unwrap();
        let target = root.0.join("late");
        fs::create_dir(&target).unwrap();
        let identity = fs::metadata(&target).unwrap().ino();
        assert!(stage.publish(path, "late", &witness, || Ok(())).is_err());
        assert_eq!(fs::metadata(&target).unwrap().ino(), identity);
        stage.discard().unwrap();
        assert!(!root.0.join(&stage.name).exists());
    }

    #[test]
    fn rsync_tree_requires_exact_content_at_verification_and_commit() {
        let root = Scratch::new();
        let path = root.0.to_str().unwrap();
        let parent = Directory::open_parent(path).unwrap();
        let mut stage = UploadStage::create(&parent).unwrap();
        let file = root.0.join(&stage.name).join(PAYLOAD).join("sample");
        let payload = b"test\0content";
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .unwrap();
        std::io::Write::write_all(&mut output, payload).unwrap();
        drop(output);
        assert!(
            stage
                .verify_inventory(path, "target", Manifest::default().finish())
                .is_err()
        );
        let mut expected = Manifest::default();
        let size = payload.len() as u64;
        expected
            .add(
                "sample",
                Some((size, hash(payload.as_slice(), size).unwrap())),
            )
            .unwrap();
        let expected = expected.finish();
        let witness = stage.verify_inventory(path, "target", expected).unwrap();
        fs::write(&file, b"test\0changed").unwrap();
        assert!(stage.publish(path, "target", &witness, || Ok(())).is_err());
        assert!(!root.0.join("target").exists());
        fs::write(&file, payload).unwrap();
        let witness = stage.verify_inventory(path, "target", expected).unwrap();
        stage.publish(path, "target", &witness, || Ok(())).unwrap();
        stage.discard().unwrap();
        assert_eq!(
            fs::read(root.0.join("target").join("sample")).unwrap(),
            payload
        );
        assert!(!root.0.join(&stage.name).exists());
    }
}
