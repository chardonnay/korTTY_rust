//! Review a held Linux file and publish only into vacant, descriptor-relative names.

use std::io::{Seek as _, Write as _};
use std::time::Duration;

use kortty_server_archive::draft_replace::{
    MAX_ORIGINAL_BYTES, Reply, Request, Review, Spec, read_request, write_reply,
};
use rustix::fs::{
    StatxFlags, XattrFlags, fchown, fgetxattr, fgetxattr_query, flistxattr, fsetxattr, statx,
};
use rustix::process::Gid;

use super::*;

const ORIGINAL: &str = "original";
const DRAFT: &str = "draft";
const BACKUP: &str = "backup";
const DEADLINE: Duration = Duration::from_secs(90);
const MAX_XATTR_BYTES: usize = 64 * 1024;

pub(crate) fn serve() -> io::Result<()> {
    std::thread::Builder::new()
        .name("kortty-draft-deadline".into())
        .spawn(|| {
            std::thread::sleep(DEADLINE);
            std::process::exit(124);
        })?;
    // This helper serves one request per process.
    umask(Mode::from_bits_truncate(0o077));
    let reply = (|| {
        if getuid() != geteuid() || getgid() != getegid() {
            return Err(invalid(
                "Server draft replacement requires an ordinary account",
            ));
        }
        let (request, draft) = read_request(io::stdin().lock())?;
        match request {
            Request::Review { spec } => review(spec).map(|review| Reply::Reviewed { review }),
            Request::Apply { spec, token } => apply(&spec, &token, &draft),
        }
    })()
    .unwrap_or_else(|error| failure(error, false, None, None));
    write_reply(io::stdout().lock(), &reply)
}

fn review(spec: Spec) -> io::Result<Review> {
    let target = Target::open(&spec)?;
    let backup = spec.backup_path()?;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let token = target.token(&hex::encode(random))?;
    require_absent(&target.parent.fd, &spec.stage_name(&token)?)?;
    target.verify()?;
    Ok(Review {
        spec,
        token,
        backup,
    })
}

fn apply(spec: &Spec, token: &str, draft: &[u8]) -> io::Result<Reply> {
    apply_with(spec, token, draft, |_| Ok(()))
}

fn apply_with(
    spec: &Spec,
    token: &str,
    draft: &[u8],
    hook: impl Fn(Phase) -> io::Result<()>,
) -> io::Result<Reply> {
    let stage_name = spec.stage_name(token)?;
    let target = Target::open(spec)?;
    if token != target.token(&token[..32])? {
        return Err(invalid(
            "The reviewed server file or its destination changed",
        ));
    }
    if target.bytes == draft {
        return Err(invalid(
            "The generated draft already matches the reviewed server file",
        ));
    }
    let recovery = spec.recovery_path(token)?;
    let mut stage = Stage::new(&target.parent, stage_name, recovery)?;
    stage.draft = Some(stage.create(DRAFT)?);
    target.write_stage(stage.draft.as_ref().expect("created draft"), draft)?;
    if target.backup_name.is_some() {
        stage.backup = Some(stage.create(BACKUP)?);
        target.write_stage(
            stage.backup.as_ref().expect("created backup"),
            &target.bytes,
        )?;
    }
    fsync(&stage.directory.fd)?;
    stage.verify(&target)?;
    target.verify()?;
    // Probe this filesystem before the first operation that can displace the target.
    match renameat_with(
        &stage.directory.fd,
        DRAFT,
        &stage.directory.fd,
        DRAFT,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(error) => return Err(error.into()),
    }
    hook(Phase::Quarantine)?;
    Ok(stage.publish(&target, &hook))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Quarantine,
    Backup,
    Publish,
}

struct Target<'a> {
    spec: &'a Spec,
    parent_path: &'a str,
    name: &'a str,
    backup_name: Option<String>,
    parent: Directory,
    mount: u64,
    file: File,
    stamp: Stamp,
    bytes: Vec<u8>,
    attributes: BTreeMap<String, Vec<u8>>,
}

impl<'a> Target<'a> {
    fn open(spec: &'a Spec) -> io::Result<Self> {
        spec.validate()?;
        let (parent_path, name) = spec.parts()?;
        let parent = Directory::open_parent(parent_path)?;
        if parent.stat.st_uid != geteuid().as_raw() {
            return Err(invalid("Choose a server folder owned by the SSH account"));
        }
        require_plain_directory(&parent.fd)?;
        let mount = mount_id(&parent.fd)?;
        let named = statat(&parent.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        require_original(&named, spec)?;
        let mut file = File::from(openat(&parent.fd, name, SOURCE, Mode::empty())?);
        let opened = fstat(&file)?;
        require_original(&opened, spec)?;
        if !same_node(&named, &opened) || mount_id(&file)? != mount {
            return Err(invalid(
                "The server file changed while opening or crosses a mount",
            ));
        }
        let stamp = Stamp::of(&file.metadata()?);
        let mut bytes = Vec::new();
        (&mut file)
            .take(spec.original.bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != spec.original.bytes || digest(&bytes) != spec.original.digest {
            return Err(invalid(
                "The server file bytes differ from the reviewed comparison",
            ));
        }
        let attributes = read_xattrs(&file)?;
        let target = Self {
            spec,
            parent_path,
            name,
            backup_name: spec.backup.then(|| format!("{name}.bak")),
            parent,
            mount,
            file,
            stamp,
            bytes,
            attributes,
        };
        target.verify()?;
        Ok(target)
    }

    fn verify_parent(&self) -> io::Result<()> {
        let current = Directory::open_parent(self.parent_path)?;
        require_plain_directory(&current.fd)?;
        if !same_node(&self.parent.stat, &current.stat)
            || current.stat.st_uid != self.parent.stat.st_uid
            || current.stat.st_gid != self.parent.stat.st_gid
            || current.stat.st_mode != self.parent.stat.st_mode
            || mount_id(&current.fd)? != self.mount
        {
            return Err(invalid("The reviewed server folder changed"));
        }
        Ok(())
    }

    fn verify(&self) -> io::Result<()> {
        self.verify_parent()?;
        self.parent.require_child(self.name, &fstat(&self.file)?)?;
        if Stamp::of(&self.file.metadata()?) != self.stamp {
            return Err(invalid("The reviewed server file changed"));
        }
        if let Some(name) = &self.backup_name {
            require_absent(&self.parent.fd, name)?;
        }
        Ok(())
    }

    fn verify_displaced(&self, directory: &Directory, displaced: &Stat) -> io::Result<()> {
        directory.require_child(ORIGINAL, displaced)?;
        let held = fstat(&self.file)?;
        require_original(&held, self.spec)?;
        let current = Stamp::of(&self.file.metadata()?);
        if !same_node(displaced, &held)
            || !same_after_rename(&self.stamp, &current)
            || file_digest(&self.file, self.spec.original.bytes)? != self.spec.original.digest
            || read_xattrs(&self.file)? != self.attributes
            || Stamp::of(&self.file.metadata()?) != current
        {
            return Err(invalid(
                "The displaced server file differs from the reviewed file",
            ));
        }
        directory.require_child(ORIGINAL, displaced)
    }

    fn token(&self, nonce: &str) -> io::Result<String> {
        let mut hash = Sha256::new();
        hash.update(b"kortty-replace-server-draft-v1\0");
        hash.update(nonce.as_bytes());
        hash.update(self.parent.stat.st_dev.to_le_bytes());
        hash.update(self.parent.stat.st_ino.to_le_bytes());
        hash.update(self.parent.stat.st_mode.to_le_bytes());
        hash.update(self.parent.stat.st_uid.to_le_bytes());
        hash.update(self.parent.stat.st_gid.to_le_bytes());
        hash.update(self.mount.to_le_bytes());
        self.stamp.hash_into(&mut hash);
        hash.update(serde_json::to_vec(self.spec).map_err(io::Error::other)?);
        Ok(format!("{nonce}{}", hex::encode(hash.finalize())))
    }

    fn write_stage(&self, mut file: &File, bytes: &[u8]) -> io::Result<()> {
        file.write_all(bytes)?;
        if file.metadata()?.gid() != self.stamp.gid {
            fchown(file, None, Some(Gid::from_raw(self.stamp.gid)))?;
        }
        for (name, bytes) in &self.attributes {
            if name.starts_with("user.") {
                fsetxattr(file, name.as_str(), bytes, XattrFlags::empty())?;
            }
        }
        fchmod(file, Mode::from_bits_truncate(self.spec.original.mode))?;
        file.sync_all()?;
        self.verify_stage_file(file, bytes.len() as u64, digest(bytes))
    }

    fn verify_stage_file(&self, file: &File, size: u64, expected: [u8; 32]) -> io::Result<()> {
        let stamp = Stamp::of(&file.metadata()?);
        if stamp.uid != self.stamp.uid
            || stamp.gid != self.stamp.gid
            || stamp.links != 1
            || stamp.mode & 0o7777 != self.spec.original.mode
            || stamp.size != size
            || mount_id(file)? != self.mount
            || read_xattrs(file)? != self.attributes
            || file_digest(file, size)? != expected
            || Stamp::of(&file.metadata()?) != stamp
        {
            return Err(invalid(
                "The server cannot preserve the reviewed file access and bytes",
            ));
        }
        Ok(())
    }
}

struct Stage<'a> {
    parent: &'a Directory,
    directory: Directory,
    name: String,
    recovery: String,
    draft: Option<File>,
    backup: Option<File>,
    keep: bool,
}

impl<'a> Stage<'a> {
    fn new(parent: &'a Directory, name: String, recovery: String) -> io::Result<Self> {
        mkdirat(&parent.fd, name.as_str(), PRIVATE_DIRECTORY)?;
        let directory = parent.child(&name)?;
        let stage = Self {
            parent,
            directory,
            name,
            recovery,
            draft: None,
            backup: None,
            keep: false,
        };
        stage.verify_directory()?;
        Ok(stage)
    }

    fn create(&self, name: &str) -> io::Result<File> {
        self.verify_directory()?;
        let file = File::from(openat(
            &self.directory.fd,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            PRIVATE_FILE,
        )?);
        self.directory.require_child(name, &fstat(&file)?)?;
        Ok(file)
    }

    fn verify_directory(&self) -> io::Result<()> {
        self.parent
            .require_child(&self.name, &self.directory.stat)?;
        let current = fstat(&self.directory.fd)?;
        if current.st_uid != geteuid().as_raw()
            || current.st_mode & 0o777 != 0o700
            || mount_id(&self.directory.fd)? != mount_id(&self.parent.fd)?
        {
            return Err(invalid("The private server draft folder changed"));
        }
        require_plain_directory(&self.directory.fd)
    }

    fn verify(&self, target: &Target<'_>) -> io::Result<()> {
        self.verify_directory()?;
        self.verify_file(
            target,
            DRAFT,
            target.spec.draft_bytes as u64,
            target.spec.draft_digest,
        )?;
        if self.backup.is_some() {
            self.verify_file(
                target,
                BACKUP,
                target.spec.original.bytes,
                target.spec.original.digest,
            )?;
        }
        require_absent(&self.directory.fd, ORIGINAL)
    }

    fn verify_file(
        &self,
        target: &Target<'_>,
        name: &str,
        bytes: u64,
        digest: [u8; 32],
    ) -> io::Result<()> {
        let file = if name == DRAFT {
            &self.draft
        } else {
            &self.backup
        };
        let file = file
            .as_ref()
            .ok_or_else(|| invalid("The private server draft file is missing"))?;
        self.directory.require_child(name, &fstat(file)?)?;
        target.verify_stage_file(file, bytes, digest)
    }

    fn publish(&mut self, target: &Target<'_>, hook: &impl Fn(Phase) -> io::Result<()>) -> Reply {
        let moved = renameat_with(
            &self.parent.fd,
            target.name,
            &self.directory.fd,
            ORIGINAL,
            RenameFlags::NOREPLACE,
        );
        if let Err(error) = moved {
            self.keep = !absent(&self.directory.fd, ORIGINAL).unwrap_or(false);
            return failure(error.into(), self.keep, self.recovery(), None);
        }
        self.keep = true;
        let displaced = match statat(&self.directory.fd, ORIGINAL, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(displaced) => displaced,
            Err(error) => return failure(error.into(), true, self.recovery(), None),
        };
        let mut backup = None;
        let mut draft_attempted = false;
        let mut backup_uncertain = false;
        let result = (|| -> io::Result<()> {
            target.verify_displaced(&self.directory, &displaced)?;
            self.verify_directory()?;
            // Persist the recovery directory before publishing either output name.
            fsync(&self.directory.fd)?;
            fsync(&self.parent.fd)?;
            if let Some(name) = &target.backup_name {
                target.verify_parent()?;
                self.verify_file(
                    target,
                    BACKUP,
                    target.spec.original.bytes,
                    target.spec.original.digest,
                )?;
                hook(Phase::Backup)?;
                let file = self.backup.as_ref().expect("verified backup");
                let expected = fstat(file)?;
                backup_uncertain = true;
                let published = renameat_with(
                    &self.directory.fd,
                    BACKUP,
                    &self.parent.fd,
                    name.as_str(),
                    RenameFlags::NOREPLACE,
                );
                if let Err(error) = published {
                    backup_uncertain = self.directory.require_child(BACKUP, &expected).is_err();
                    return Err(error.into());
                }
                backup = target.spec.backup_path()?;
                self.parent.require_child(name, &expected)?;
                target.verify_stage_file(
                    file,
                    target.spec.original.bytes,
                    target.spec.original.digest,
                )?;
                backup_uncertain = false;
            }
            target.verify_parent()?;
            target.verify_displaced(&self.directory, &displaced)?;
            self.verify_directory()?;
            self.verify_file(
                target,
                DRAFT,
                target.spec.draft_bytes as u64,
                target.spec.draft_digest,
            )?;
            hook(Phase::Publish)?;
            let file = self.draft.as_ref().expect("verified draft");
            let expected = fstat(file)?;
            draft_attempted = true;
            let published = renameat_with(
                &self.directory.fd,
                DRAFT,
                &self.parent.fd,
                target.name,
                RenameFlags::NOREPLACE,
            );
            if let Err(error) = published {
                draft_attempted = self.directory.require_child(DRAFT, &expected).is_err();
                return Err(error.into());
            }
            self.parent.require_child(target.name, &expected)?;
            target.verify_stage_file(
                file,
                target.spec.draft_bytes as u64,
                target.spec.draft_digest,
            )?;
            target.verify_parent()?;
            if let (Some(name), Some(file)) = (&target.backup_name, &self.backup) {
                self.parent.require_child(name, &fstat(file)?)?;
            }
            fsync(&self.directory.fd)?;
            fsync(&self.parent.fd)?;
            Ok(())
        })();
        if let Err(error) = result {
            if !draft_attempted {
                self.keep = self.restore(target, &displaced).is_err();
            }
            return failure(
                error,
                draft_attempted || backup_uncertain || self.keep,
                self.recovery(),
                backup,
            );
        }
        let cleanup = self
            .directory
            .require_child(ORIGINAL, &displaced)
            .and_then(|()| {
                unlinkat(&self.directory.fd, ORIGINAL, AtFlags::empty()).map_err(Into::into)
            });
        self.keep = cleanup.is_err();
        Reply::Applied {
            destination: target.spec.destination.clone(),
            draft_digest: target.spec.draft_digest,
            backup,
            recovery: self.recovery(),
        }
    }

    fn restore(&self, target: &Target<'_>, displaced: &Stat) -> io::Result<()> {
        target.verify_parent()?;
        self.verify_directory()?;
        self.directory.require_child(ORIGINAL, displaced)?;
        renameat_with(
            &self.directory.fd,
            ORIGINAL,
            &self.parent.fd,
            target.name,
            RenameFlags::NOREPLACE,
        )?;
        self.parent.require_child(target.name, displaced)?;
        fsync(&self.directory.fd)?;
        fsync(&self.parent.fd)?;
        Ok(())
    }

    fn recovery(&self) -> Option<String> {
        self.keep.then(|| self.recovery.clone())
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if self.keep || self.verify_directory().is_err() {
            return;
        }
        for (name, file) in [(DRAFT, &self.draft), (BACKUP, &self.backup)] {
            if let Some(file) = file
                && fstat(file).is_ok_and(|stamp| self.directory.require_child(name, &stamp).is_ok())
            {
                let _ = unlinkat(&self.directory.fd, name, AtFlags::empty());
            }
        }
        if self
            .parent
            .require_child(&self.name, &self.directory.stat)
            .is_ok()
            && unlinkat(&self.parent.fd, self.name.as_str(), AtFlags::REMOVEDIR).is_ok()
        {
            let _ = fsync(&self.parent.fd);
        }
    }
}

fn require_original(stat: &Stat, spec: &Spec) -> io::Result<()> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != geteuid().as_raw()
        || stat.st_uid != spec.original.uid
        || stat.st_gid != spec.original.gid
        || stat.st_nlink != 1
        || stat.st_mode & 0o7777 != spec.original.mode
        || u64::try_from(stat.st_size).ok() != Some(spec.original.bytes)
    {
        return Err(invalid(
            "Choose a reviewed single-link file owned by the SSH account with ordinary permissions",
        ));
    }
    Ok(())
}

fn require_plain_directory(file: &File) -> io::Result<()> {
    for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
        match fgetxattr_query(file, attribute) {
            Err(Errno::NODATA | Errno::NOTSUP) => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(invalid(
                    "The server draft directory has an extended POSIX ACL",
                ));
            }
        }
    }
    Ok(())
}

fn read_xattrs(file: &File) -> io::Result<BTreeMap<String, Vec<u8>>> {
    let mut names = [0; 4096];
    let length = match flistxattr(file, &mut names) {
        Ok(length) => length,
        Err(Errno::NOTSUP) => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    let mut attributes = BTreeMap::new();
    let mut total = 0;
    for name in names[..length]
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name =
            std::str::from_utf8(name).map_err(|_| invalid("Unsupported server file attributes"))?;
        if name != "security.selinux" && !name.starts_with("user.") {
            return Err(invalid(
                "The server file has security attributes that cannot be preserved",
            ));
        }
        let size = fgetxattr_query(file, name)?;
        total += size;
        if total > MAX_XATTR_BYTES {
            return Err(invalid("The server file attributes exceed their limit"));
        }
        let mut value = vec![0; size.max(1)];
        if fgetxattr(file, name, value.as_mut_slice())? != size {
            return Err(invalid("The reviewed server file attributes changed"));
        }
        value.truncate(size);
        attributes.insert(name.to_owned(), value);
    }
    Ok(attributes)
}

fn mount_id(file: &File) -> io::Result<u64> {
    let info = statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
    if info.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(invalid("The server cannot verify the mount boundary"));
    }
    Ok(info.stx_mnt_id)
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn file_digest(mut file: &File, size: u64) -> io::Result<[u8; 32]> {
    if size > MAX_ORIGINAL_BYTES {
        return Err(invalid("The server file exceeds its size limit"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    let mut remaining = size;
    file.rewind()?;
    while remaining != 0 {
        let length = buffer.len().min(remaining as usize);
        let count = file.read(&mut buffer[..length])?;
        if count == 0 {
            return Err(invalid("The reviewed server file was truncated"));
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if file.read(&mut buffer[..1])? != 0 {
        return Err(invalid("The reviewed server file grew"));
    }
    Ok(hash.finalize().into())
}

fn same_after_rename(expected: &Stamp, current: &Stamp) -> bool {
    expected.dev == current.dev
        && expected.ino == current.ino
        && expected.mode == current.mode
        && expected.links == current.links
        && expected.uid == current.uid
        && expected.gid == current.gid
        && expected.size == current.size
        && expected.mtime == current.mtime
        && expected.mtime_ns == current.mtime_ns
}

fn failure(
    error: io::Error,
    outcome_unknown: bool,
    recovery: Option<String>,
    backup: Option<String>,
) -> Reply {
    Reply::Failed {
        message: Failure::from(error).message,
        outcome_unknown,
        recovery,
        backup,
    }
}

#[cfg(test)]
mod tests;
