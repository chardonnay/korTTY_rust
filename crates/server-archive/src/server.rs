use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;

use kortty_server_archive::decode::visit;
use kortty_server_archive::manifest::{
    MAX_COMPONENT_BYTES, MAX_DEPTH, MAX_PATH_BYTES, Manifest, Member, MemberKind,
};
use kortty_server_archive::protocol::{Reply, Request, Review};
use kortty_server_archive::{Format, MAX_ARCHIVE_BYTES};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat, fchmod, fstat, fsync, linkat, mkdirat,
    open, openat, renameat_with, statat, symlinkat, unlinkat,
};
use rustix::io::Errno;
use rustix::process::{getegid, geteuid, getgid, getuid, umask};
use sha2::{Digest, Sha256};

mod create;
#[cfg(target_os = "linux")]
pub(super) mod draft_replace;
#[cfg(target_os = "linux")]
pub(super) mod privileged_archive;
pub(super) mod rsync_tree;

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
const SOURCE: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOCTTY);
const PRIVATE_DIRECTORY: Mode = Mode::RUSR.union(Mode::WUSR).union(Mode::XUSR);
const PRIVATE_FILE: Mode = Mode::RUSR.union(Mode::WUSR);
const TARGET_COUNT: usize = 100;

pub(super) fn handle(request: Request) -> Reply {
    if getuid() != geteuid() || getgid() != getegid() {
        return Failure::from(invalid(
            "The archive helper requires ordinary account privileges",
        ))
        .reply();
    }
    // This binary handles one request and never shares its process umask.
    umask(Mode::from_bits_truncate(0o077));
    match request {
        Request::Review { source } => {
            review(source).map_or_else(Failure::reply, |review| Reply::Review { review })
        }
        Request::Apply {
            source,
            destination,
            token,
        } => apply(&source, destination, &token)
            .map_or_else(Failure::reply, |destination| Reply::Applied { destination }),
        Request::ReviewCreation { spec } => create::review(spec)
            .map_or_else(Failure::reply, |review| Reply::CreationReview { review }),
        Request::ApplyCreation { spec, token } => create::apply(spec, &token)
            .map_or_else(Failure::reply, |destination| Reply::Applied { destination }),
    }
}

fn review(source: String) -> Result<Review, Failure> {
    let archive = Archive::open(&source)?;
    let manifest = visit(archive.format, &archive.bytes, |_, _| Ok(()))?;
    archive.verify()?;
    let target = archive
        .candidates()
        .find_map(|candidate| match absent(&archive.parent.fd, &candidate) {
            Ok(true) => Some(Ok(candidate)),
            Ok(false) => None,
            Err(error) => Some(Err(error)),
        })
        .ok_or_else(|| invalid("No unused sibling folder is available"))??;
    let destination = archive.destination(&target);
    let token = archive.token(&destination);
    Ok(Review {
        source,
        destination,
        token,
        manifest,
    })
}

fn apply(source: &str, destination: String, token: &str) -> Result<String, Failure> {
    let archive = Archive::open(source)?;
    let target = archive
        .candidates()
        .find(|name| archive.destination(name) == destination)
        .ok_or_else(|| invalid("The reviewed destination is not an eligible sibling folder"))?;
    if archive.token(&destination) != token {
        return Err(invalid("The reviewed server archive or destination changed").into());
    }
    require_absent(&archive.parent.fd, &target)?;
    let mut stage = Stage::create(&archive.parent)?;
    let result = (|| {
        let manifest = visit(archive.format, &archive.bytes, |member, reader| {
            stage.write_member(member, reader)
        })?;
        stage.write_links(&manifest)?;
        stage.verify_and_sync(&manifest)?;
        archive.verify()?;
        stage.publish(&target)?;
        archive.verify_parent()
    })();
    match result {
        Ok(()) => Ok(destination),
        Err(error) => Err(stage.fail(error)),
    }
}

struct Archive<'a> {
    source: &'a str,
    parent_path: &'a str,
    name: &'a str,
    stem: &'a str,
    format: Format,
    parent: Directory,
    file: File,
    initial: Stamp,
    bytes: Vec<u8>,
    digest: [u8; 32],
}

impl<'a> Archive<'a> {
    fn open(source: &'a str) -> io::Result<Self> {
        let (parent_path, name) = absolute_file(source)?;
        let (format, stem) = Format::detect(name)
            .ok_or_else(|| invalid("This server archive format is unsupported"))?;
        let parent = Directory::open_parent(parent_path)?;
        let named = statat(&parent.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if !protected_file(&named) {
            return Err(invalid("Choose a protected regular server archive"));
        }
        let mut file = File::from(openat(&parent.fd, name, SOURCE, Mode::empty())?);
        let opened = fstat(&file)?;
        if !same_node(&named, &opened) || !protected_file(&opened) {
            return Err(invalid("The server archive changed while opening"));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(&file)?;
        let initial = Stamp::of(&file.metadata()?);
        if !(1..=MAX_ARCHIVE_BYTES).contains(&initial.size) {
            return Err(invalid(
                "The server archive exceeds its size limit or is empty",
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(usize::try_from(initial.size + 1).map_err(io::Error::other)?)
            .map_err(|_| invalid("The server archive exceeds available memory"))?;
        (&mut file).take(initial.size + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).ok() != Some(initial.size) {
            return Err(invalid("The server archive changed while reading"));
        }
        let digest = Sha256::digest(&bytes).into();
        let archive = Self {
            source,
            parent_path,
            name,
            stem: stem.trim(),
            format,
            parent,
            file,
            initial,
            bytes,
            digest,
        };
        archive.verify()?;
        Ok(archive)
    }

    fn verify_parent(&self) -> io::Result<()> {
        let current = Directory::open_parent(self.parent_path)?;
        if same_node(&current.stat, &self.parent.stat)
            && current.stat.st_mode == self.parent.stat.st_mode
            && current.stat.st_uid == self.parent.stat.st_uid
            && current.stat.st_gid == self.parent.stat.st_gid
        {
            Ok(())
        } else {
            Err(invalid("The reviewed server folder changed"))
        }
    }

    fn verify(&self) -> io::Result<()> {
        self.verify_parent()?;
        let named = statat(&self.parent.fd, self.name, AtFlags::SYMLINK_NOFOLLOW)?;
        let opened = fstat(&self.file)?;
        if !protected_file(&named)
            || !same_node(&named, &opened)
            || Stamp::of(&self.file.metadata()?) != self.initial
        {
            return Err(invalid("The reviewed server archive changed"));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(&self.file)?;
        Ok(())
    }

    fn candidates(&self) -> impl Iterator<Item = String> + '_ {
        let stem = if valid_component(self.stem) {
            self.stem
        } else {
            "extracted"
        };
        (0..TARGET_COUNT).filter_map(move |index| {
            let name = if index == 0 {
                stem.to_owned()
            } else {
                format!("{stem} ({index})")
            };
            (valid_component(&name) && self.destination(&name).len() <= MAX_PATH_BYTES)
                .then_some(name)
        })
    }

    fn destination(&self, target: &str) -> String {
        format!("{}/{target}", self.parent_path.trim_end_matches('/'))
    }

    fn token(&self, destination: &str) -> String {
        let mut hash = Sha256::new();
        hash.update(b"kortty server archive review v1\0");
        hash.update(self.source.as_bytes());
        hash.update(b"\0");
        hash.update(destination.as_bytes());
        hash.update(b"\0");
        hash.update(self.parent.stat.st_dev.to_le_bytes());
        hash.update(self.parent.stat.st_ino.to_le_bytes());
        hash.update(self.parent.stat.st_mode.to_le_bytes());
        hash.update(self.parent.stat.st_uid.to_le_bytes());
        hash.update(self.parent.stat.st_gid.to_le_bytes());
        self.initial.hash_into(&mut hash);
        hash.update(self.digest);
        hex::encode(hash.finalize())
    }
}

#[derive(PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

impl Stamp {
    fn of(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            size: metadata.size(),
            mtime: metadata.mtime(),
            mtime_ns: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_ns: metadata.ctime_nsec(),
        }
    }

    fn hash_into(&self, hash: &mut Sha256) {
        hash.update(self.dev.to_le_bytes());
        hash.update(self.ino.to_le_bytes());
        hash.update(self.mode.to_le_bytes());
        hash.update(self.links.to_le_bytes());
        hash.update(self.uid.to_le_bytes());
        hash.update(self.gid.to_le_bytes());
        hash.update(self.size.to_le_bytes());
        hash.update(self.mtime.to_le_bytes());
        hash.update(self.mtime_ns.to_le_bytes());
        hash.update(self.ctime.to_le_bytes());
        hash.update(self.ctime_ns.to_le_bytes());
    }
}

struct Directory {
    fd: File,
    stat: Stat,
}

impl Directory {
    fn open_parent(path: &str) -> io::Result<Self> {
        let mut directory = Self::from_fd(open("/", DIRECTORY, Mode::empty())?)?;
        if path != "/" {
            for component in path[1..].split('/') {
                directory.require_protected(true)?;
                directory = directory.child(component)?;
            }
        }
        directory.require_protected(false)?;
        Ok(directory)
    }

    fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        let stat = fstat(&fd)?;
        Ok(Self {
            fd: File::from(fd),
            stat,
        })
    }

    fn child(&self, name: &str) -> io::Result<Self> {
        Self::from_fd(openat(&self.fd, name, DIRECTORY, Mode::empty())?)
    }

    fn require_child(&self, name: &str, expected: &Stat) -> io::Result<()> {
        let current = statat(&self.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if same_node(&current, expected) {
            Ok(())
        } else {
            Err(invalid("A private extraction path changed"))
        }
    }

    fn require_protected(&self, ancestor: bool) -> io::Result<()> {
        let writable = self.stat.st_mode & 0o022 != 0;
        let sticky_root = self.stat.st_uid == 0 && self.stat.st_mode & 0o1000 != 0;
        if trusted_owner(&self.stat) && (!writable || (ancestor && sticky_root)) {
            #[cfg(target_os = "macos")]
            kortty_files::require_no_granting_extended_acl(&self.fd)?;
            Ok(())
        } else {
            Err(invalid("Choose a protected canonical server folder"))
        }
    }
}

fn make_private(file: &File, mode: Mode) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    kortty_files::require_no_extended_acl(file)?;
    fchmod(file, mode)?;
    Ok(())
}

fn trusted_owner(stat: &Stat) -> bool {
    stat.st_uid == 0 || stat.st_uid == geteuid().as_raw()
}

fn protected_file(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
        && trusted_owner(stat)
        && stat.st_mode & 0o022 == 0
}

fn same_node(first: &Stat, second: &Stat) -> bool {
    first.st_dev == second.st_dev
        && first.st_ino == second.st_ino
        && FileType::from_raw_mode(first.st_mode) == FileType::from_raw_mode(second.st_mode)
}

fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_COMPONENT_BYTES
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

fn absolute_file(path: &str) -> io::Result<(&str, &str)> {
    if path.len() <= MAX_PATH_BYTES
        && let Some(components) = path.strip_prefix('/')
        && components.split('/').all(valid_component)
        && let Some((parent, name)) = path.rsplit_once('/')
    {
        return Ok((if parent.is_empty() { "/" } else { parent }, name));
    }
    Err(invalid("Choose a canonical absolute server archive path"))
}

fn absent(parent: &File, name: &str) -> io::Result<bool> {
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(false),
        Err(Errno::NOENT) => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn require_absent(parent: &File, name: &str) -> io::Result<()> {
    if absent(parent, name)? {
        Ok(())
    } else {
        Err(invalid("The reviewed server destination already exists"))
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::other(message)
}

struct Stage<'a> {
    parent: &'a Directory,
    name: String,
    directory: Directory,
    entries: BTreeMap<String, Stat>,
    owned: bool,
    published: bool,
}

impl<'a> Stage<'a> {
    fn create(parent: &'a Directory) -> io::Result<Self> {
        let mut random = [0; 16];
        getrandom::fill(&mut random)
            .map_err(|_| invalid("Could not generate a private extraction folder name"))?;
        let name = format!(".kortty-extract.{}", hex::encode(random));
        mkdirat(&parent.fd, name.as_str(), PRIVATE_DIRECTORY)?;
        let named = statat(&parent.fd, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
        let directory = parent.child(&name)?;
        if !same_node(&named, &directory.stat) || directory.stat.st_uid != geteuid().as_raw() {
            return Err(invalid(
                "The private extraction folder changed while opening",
            ));
        }
        let stage = Self {
            parent,
            name,
            directory,
            entries: BTreeMap::new(),
            owned: true,
            published: false,
        };
        make_private(&stage.directory.fd, PRIVATE_DIRECTORY)?;
        Ok(stage)
    }

    fn write_member(&mut self, member: &Member, reader: &mut dyn Read) -> io::Result<()> {
        let (parent, name) = member_path(&member.path)?;
        match &member.kind {
            MemberKind::Directory => {
                self.ensure_folders(&member.path)?;
            }
            MemberKind::File => {
                let directory = self.ensure_folders(parent)?;
                let mut file = File::from(openat(
                    &directory.fd,
                    name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    PRIVATE_FILE,
                )?);
                self.entries.insert(member.path.clone(), fstat(&file)?);
                make_private(&file, PRIVATE_FILE)?;
                let length = member
                    .size
                    .checked_add(1)
                    .ok_or_else(|| invalid("An archive member exceeds its size limit"))?;
                if io::copy(&mut reader.take(length), &mut file)? != member.size {
                    return Err(invalid("An archive member changed length while extracting"));
                }
                file.sync_all()?;
                self.entries.insert(member.path.clone(), fstat(&file)?);
            }
            MemberKind::Symlink { .. } | MemberKind::Hardlink { .. } => {
                return Err(invalid(
                    "An archive link arrived before inventory verification",
                ));
            }
        }
        Ok(())
    }

    fn ensure_folders(&mut self, path: &str) -> io::Result<Directory> {
        let mut directory = self.directory.child(".")?;
        let mut current = String::new();
        for name in path.split('/').filter(|name| !name.is_empty()) {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(name);
            if let Some(expected) = self.entries.get(&current) {
                directory.require_child(name, expected)?;
                directory = directory.child(name)?;
                if !same_node(&directory.stat, expected) {
                    return Err(invalid("A private extraction folder changed"));
                }
            } else {
                mkdirat(&directory.fd, name, PRIVATE_DIRECTORY)?;
                let named = statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
                directory = directory.child(name)?;
                if !same_node(&named, &directory.stat)
                    || directory.stat.st_uid != geteuid().as_raw()
                {
                    return Err(invalid("A private extraction folder changed while opening"));
                }
                self.entries.insert(current.clone(), named);
                make_private(&directory.fd, PRIVATE_DIRECTORY)?;
                self.entries.insert(current.clone(), fstat(&directory.fd)?);
            }
        }
        Ok(directory)
    }

    fn existing_folder(&self, path: &str) -> io::Result<Directory> {
        let mut directory = self.directory.child(".")?;
        let mut current = String::new();
        for name in path.split('/').filter(|name| !name.is_empty()) {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(name);
            let expected = self
                .entries
                .get(&current)
                .ok_or_else(|| invalid("An expected private extraction folder is missing"))?;
            directory = directory.child(name)?;
            if !same_node(&directory.stat, expected) {
                return Err(invalid("A private extraction folder changed"));
            }
        }
        Ok(directory)
    }

    fn write_links(&mut self, manifest: &Manifest) -> io::Result<()> {
        for member in &manifest.members {
            let (parent, name) = member_path(&member.path)?;
            match &member.kind {
                MemberKind::Symlink { target } => {
                    let relative = relative_target(parent, target)?;
                    let directory = self.ensure_folders(parent)?;
                    symlinkat(relative.as_str(), &directory.fd, name)?;
                    let stat = statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
                    self.entries.insert(member.path.clone(), stat);
                }
                MemberKind::Hardlink { target } => {
                    let directory = self.ensure_folders(parent)?;
                    let (target_parent, target_name) = member_path(target)?;
                    let origin = self.existing_folder(target_parent)?;
                    let expected = self.entries.get(target).ok_or_else(|| {
                        invalid("An archive hard link has no verified regular target")
                    })?;
                    if FileType::from_raw_mode(expected.st_mode) != FileType::RegularFile {
                        return Err(invalid("An archive hard link target is not regular"));
                    }
                    origin.require_child(target_name, expected)?;
                    linkat(
                        &origin.fd,
                        target_name,
                        &directory.fd,
                        name,
                        AtFlags::empty(),
                    )?;
                    let stat = statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
                    let matches = same_node(&stat, expected);
                    self.entries.insert(member.path.clone(), stat);
                    if !matches {
                        return Err(invalid("An archive hard link target changed"));
                    }
                }
                MemberKind::Directory | MemberKind::File => {}
            }
        }
        Ok(())
    }

    fn verify_and_sync(&self, manifest: &Manifest) -> io::Result<()> {
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        let stat = fstat(&self.directory.fd)?;
        if stat.st_uid != geteuid().as_raw() || stat.st_mode & 0o7777 != 0o700 {
            return Err(invalid("The private extraction folder permissions changed"));
        }
        if self.entries.len() != manifest.members.len()
            || !manifest.members.iter().all(|member| {
                self.entries.get(&member.path).is_some_and(|stat| {
                    let kind = FileType::from_raw_mode(stat.st_mode);
                    match &member.kind {
                        MemberKind::Directory => kind == FileType::Directory,
                        MemberKind::File => {
                            kind == FileType::RegularFile
                                && u64::try_from(stat.st_size).ok() == Some(member.size)
                        }
                        MemberKind::Hardlink { .. } => kind == FileType::RegularFile,
                        MemberKind::Symlink { .. } => kind == FileType::Symlink,
                    }
                })
            })
        {
            return Err(invalid(
                "The private extraction tree differs from its inventory",
            ));
        }
        let mut remaining = self.entries.len();
        self.verify_folder(&self.directory, "", &mut remaining)?;
        if remaining != 0 {
            return Err(invalid("An expected private extraction entry is missing"));
        }
        Ok(())
    }

    fn verify_folder(
        &self,
        directory: &Directory,
        prefix: &str,
        remaining: &mut usize,
    ) -> io::Result<()> {
        for entry in Dir::read_from(&directory.fd)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .to_str()
                .map_err(|_| invalid("Unexpected name in the private extraction folder"))?;
            if matches!(name, "." | "..") {
                continue;
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| invalid("Unexpected entry in the private extraction folder"))?;
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let current = statat(&directory.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
            let expected = self
                .entries
                .get(&path)
                .ok_or_else(|| invalid("Unexpected entry in the private extraction folder"))?;
            let kind = FileType::from_raw_mode(current.st_mode);
            if !same_node(&current, expected)
                || current.st_mode != expected.st_mode
                || current.st_uid != expected.st_uid
                || current.st_gid != expected.st_gid
                || (kind != FileType::Directory && current.st_size != expected.st_size)
            {
                return Err(invalid("A private extraction entry changed"));
            }
            if kind == FileType::Directory {
                let child = directory.child(name)?;
                if !same_node(&child.stat, expected) {
                    return Err(invalid("A private extraction folder changed"));
                }
                self.verify_folder(&child, &path, remaining)?;
            }
        }
        fsync(&directory.fd)?;
        Ok(())
    }

    fn publish(&mut self, target: &str) -> io::Result<()> {
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        match renameat_with(
            &self.parent.fd,
            self.name.as_str(),
            &self.parent.fd,
            target,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {
                self.owned = false;
                self.published = true;
            }
            Err(Errno::EXIST) => {
                return Err(invalid("The reviewed server destination already exists"));
            }
            Err(Errno::NOSYS | Errno::NOTSUP | Errno::INVAL) => {
                return Err(invalid(
                    "Atomic extraction publication is unavailable on this filesystem",
                ));
            }
            Err(error) => {
                // An I/O failure can obscure the rename result on a network filesystem.
                if self
                    .parent
                    .require_child(&self.name, &self.directory.stat)
                    .is_err()
                {
                    self.owned = false;
                    self.published = true;
                }
                return Err(error.into());
            }
        }
        self.parent.require_child(target, &self.directory.stat)?;
        fsync(&self.parent.fd)?;
        Ok(())
    }

    fn fail(&mut self, error: io::Error) -> Failure {
        let mut failure = Failure::from(error);
        failure.outcome_unknown = self.published;
        if self.owned && self.discard().is_err() {
            failure
                .message
                .push_str(". A private extraction folder may remain");
        }
        failure
    }

    fn discard(&mut self) -> io::Result<()> {
        self.owned = false;
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        let mut failure = None;
        // Every descendant sorts after its parent, so removal stays depth first.
        for (path, expected) in self.entries.iter().rev() {
            let result = (|| {
                let (parent, name) = member_path(path)?;
                let directory = self.existing_folder(parent)?;
                directory.require_child(name, expected)?;
                let flags = if FileType::from_raw_mode(expected.st_mode) == FileType::Directory {
                    AtFlags::REMOVEDIR
                } else {
                    AtFlags::empty()
                };
                Ok(unlinkat(&directory.fd, name, flags)?)
            })();
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        unlinkat(&self.parent.fd, self.name.as_str(), AtFlags::REMOVEDIR)?;
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if self.owned {
            let _ = self.discard();
        }
    }
}

fn member_path(path: &str) -> io::Result<(&str, &str)> {
    let mut components = path.split('/');
    if path.len() > MAX_PATH_BYTES
        || components.clone().count() > MAX_DEPTH
        || !components.all(valid_component)
    {
        return Err(invalid(
            "An archive member path is unsafe or exceeds its limit",
        ));
    }
    Ok(path.rsplit_once('/').unwrap_or(("", path)))
}

fn relative_target(parent: &str, target: &str) -> io::Result<String> {
    let depth = if parent.is_empty() {
        0
    } else {
        parent.split('/').count()
    };
    let mut relative = "../".repeat(depth);
    if target.is_empty() {
        if relative.pop().is_none() {
            relative.push('.');
        }
    } else {
        member_path(target)?;
        relative.push_str(target);
    }
    Ok(relative)
}

struct Failure {
    message: String,
    outcome_unknown: bool,
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self {
            message: error
                .to_string()
                .chars()
                .filter(|character| !character.is_control())
                .take(320)
                .collect(),
            outcome_unknown: false,
        }
    }
}

impl Failure {
    fn reply(self) -> Reply {
        Reply::Failed {
            message: self.message,
            outcome_unknown: self.outcome_unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let root = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("kortty-archive-stage-{}", hex::encode(nonce)));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            Self(root)
        }

        fn directory(&self) -> Directory {
            Directory::open_parent(self.0.to_str().unwrap()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_publication_preserves_a_late_empty_directory() {
        let scratch = Scratch::new();
        let parent = scratch.directory();
        let mut stage = Stage::create(&parent).unwrap();
        let file = Member {
            path: "child".into(),
            kind: MemberKind::File,
            size: 1,
        };
        stage.write_member(&file, &mut &b"x"[..]).unwrap();
        stage
            .verify_and_sync(&Manifest {
                members: vec![file],
                expanded_bytes: 1,
            })
            .unwrap();

        let target = scratch.0.join("reviewed");
        fs::create_dir(&target).unwrap();
        let before = fs::metadata(&target).unwrap();
        let error = stage.publish("reviewed").unwrap_err();
        let failure = stage.fail(error);
        let after = fs::metadata(&target).unwrap();
        assert!(!failure.outcome_unknown);
        assert_eq!(before.ino(), after.ino());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
    }

    #[test]
    fn cleanup_refuses_to_unlink_a_substituted_stage_member() {
        let scratch = Scratch::new();
        let parent = scratch.directory();
        let mut stage = Stage::create(&parent).unwrap();
        let file = Member {
            path: "child".into(),
            kind: MemberKind::File,
            size: 1,
        };
        stage.write_member(&file, &mut &b"x"[..]).unwrap();
        let path = scratch.0.join(&stage.name).join("child");
        let original = path.with_file_name("original");
        fs::rename(&path, &original).unwrap();
        fs::write(&path, "unrelated").unwrap();
        let failure = stage.fail(invalid("Test stop"));
        assert!(failure.message.contains("may remain"));
        assert_eq!(fs::read(&path).unwrap(), b"unrelated");
        assert_eq!(fs::read(&original).unwrap(), b"x");
    }
}
