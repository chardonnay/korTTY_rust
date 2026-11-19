use std::os::fd::AsRawFd as _;
use std::time::{Duration, Instant};

use kortty_server_archive::rsync_tree::{Inventory, MAX_DEPTH, Manifest, valid_name};
use rustix::fs::fchown;
use rustix::process::{Gid, Uid};

use super::super::super::draft_replace::mount_id;
use super::super::Rules;
use super::super::handoff::{Kind, Lease, PAYLOAD};
use super::*;

pub(super) struct Handoff<'a> {
    rules: &'a Rules,
    lease: Lease,
    payload: Directory,
    path: String,
    owned: bool,
}

impl<'a> Handoff<'a> {
    pub(super) fn create(rules: &'a Rules) -> io::Result<Self> {
        let (root_path, root, mount) = rules.handoff()?;
        let mut lease = Lease::create(rules, root, mount, Kind::Upload)?;
        let directory = &lease.directory;
        let setup = (|| {
            lease.require_current()?;
            mkdirat(&directory.fd, PAYLOAD, PRIVATE_DIRECTORY)?;
            let payload = directory.child(PAYLOAD)?;
            let (uid, gid) = rules.login();
            fchown(
                &payload.fd,
                Some(Uid::from_raw(uid)),
                Some(Gid::from_raw(gid)),
            )?;
            require_login(&payload.fd, true, mount, (uid, gid))?;
            fchmod(&directory.fd, Mode::from_bits_truncate(0o711))?;
            Ok(payload)
        })();
        let payload = match setup {
            Ok(payload) => payload,
            Err(error) => {
                let _ = lease.remove(rules.login());
                return Err(error);
            }
        };
        let path = format!("{root_path}/{}/{PAYLOAD}", lease.name);
        Ok(Self {
            rules,
            lease,
            payload,
            path,
            owned: true,
        })
    }

    pub(super) fn path(&self) -> &str {
        &self.path
    }

    fn require_current(&self) -> io::Result<()> {
        let (_, root, mount) = self.rules.handoff()?;
        if !same_node(&root.stat, &self.lease.parent.stat) || mount != self.lease.mount {
            return Err(changed());
        }
        self.lease.require_current()?;
        self.lease
            .directory
            .require_child(PAYLOAD, &self.payload.stat)?;
        require_login(&self.payload.fd, true, self.lease.mount, self.rules.login())
    }

    pub(super) fn copy(
        &self,
        target: &Directory,
        directory: bool,
        expected: Inventory,
        permit: &impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        self.require_current()?;
        permit()?;
        let mut copy = CopyTree {
            rules: self.rules,
            permit,
            mount: self.lease.mount,
            manifest: Manifest::default(),
            limit: expected,
            files_left: expected.files,
            entries_left: expected.entries,
            bytes_left: expected.bytes,
            checked: Instant::now(),
        };
        let before = Stamp::of(&self.payload.fd.metadata()?);
        if directory {
            copy.directory(&self.payload, target, "", 0)?;
        } else {
            let names = names(&self.payload, 1)?;
            let [name] = names.as_slice() else {
                return Err(changed());
            };
            let source = hold(&self.payload, name)?;
            copy.file(&self.payload, name, source, target, FILE, "")?;
        }
        if Stamp::of(&self.payload.fd.metadata()?) != before || copy.manifest.finish() != copy.limit
        {
            return Err(changed());
        }
        self.require_current()?;
        permit()?;
        self.rules.require_current()
    }

    pub(super) fn discard(&mut self) -> io::Result<()> {
        if !self.owned {
            return Ok(());
        }
        self.owned = false;
        self.lease
            .directory
            .require_child(PAYLOAD, &self.payload.stat)?;
        self.lease.remove(self.rules.login())
    }
}

impl Drop for Handoff<'_> {
    fn drop(&mut self) {
        let _ = self.discard();
    }
}

struct CopyTree<'a> {
    rules: &'a Rules,
    permit: &'a dyn Fn() -> io::Result<()>,
    mount: u64,
    manifest: Manifest,
    limit: Inventory,
    files_left: u32,
    entries_left: u32,
    bytes_left: u64,
    checked: Instant,
}

impl CopyTree<'_> {
    fn checkpoint(&mut self) -> io::Result<()> {
        if self.checked.elapsed() >= Duration::from_millis(250) {
            self.rules.require_current()?;
            (self.permit)()?;
            self.checked = Instant::now();
        }
        Ok(())
    }

    fn directory(
        &mut self,
        source: &Directory,
        target: &Directory,
        prefix: &str,
        depth: usize,
    ) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(changed());
        }
        self.checkpoint()?;
        require_login(&source.fd, true, self.mount, self.rules.login())?;
        let before = Stamp::of(&source.fd.metadata()?);
        for name in names(source, self.entries_left as usize)? {
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let held = hold(source, &name)?;
            let stat = fstat(&held)?;
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    self.take_entry()?;
                    self.manifest.add(&relative, None)?;
                    let source_directory = source.child(&name)?;
                    if !same_node(&source_directory.stat, &stat) {
                        return Err(changed());
                    }
                    (self.permit)()?;
                    mkdirat(&target.fd, name.as_str(), PRIVATE_DIRECTORY)?;
                    let target_directory = target.child(&name)?;
                    self.directory(&source_directory, &target_directory, &relative, depth + 1)?;
                    source.require_child(&name, &stat)?;
                }
                FileType::RegularFile => {
                    self.file(source, &name, held, target, &name, &relative)?
                }
                _ => return Err(changed()),
            }
        }
        if Stamp::of(&source.fd.metadata()?) != before {
            return Err(changed());
        }
        fsync(&target.fd)?;
        Ok(())
    }

    fn take_entry(&mut self) -> io::Result<()> {
        self.entries_left = self.entries_left.checked_sub(1).ok_or_else(changed)?;
        Ok(())
    }

    fn file(
        &mut self,
        source: &Directory,
        name: &str,
        held: File,
        target: &Directory,
        target_name: &str,
        relative: &str,
    ) -> io::Result<()> {
        self.take_entry()?;
        self.files_left = self.files_left.checked_sub(1).ok_or_else(changed)?;
        require_login(&held, false, self.mount, self.rules.login())?;
        let before = Stamp::of(&held.metadata()?);
        self.bytes_left = self
            .bytes_left
            .checked_sub(before.size)
            .ok_or_else(changed)?;
        // Reopen only the checked O_PATH descriptor. A substituted named device
        // must never be opened as root, even with nonblocking flags.
        let mut input = File::from(open(
            format!("/proc/self/fd/{}", held.as_raw_fd()),
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        if Stamp::of(&input.metadata()?) != before {
            return Err(changed());
        }
        (self.permit)()?;
        let mut output = File::from(openat(
            &target.fd,
            target_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            PRIVATE_FILE,
        )?);
        let mut hash = Sha256::new();
        let mut remaining = before.size;
        let mut buffer = [0; 32 * 1024];
        while remaining != 0 {
            self.checkpoint()?;
            let length = remaining.min(buffer.len() as u64) as usize;
            input.read_exact(&mut buffer[..length])?;
            output.write_all(&buffer[..length])?;
            hash.update(&buffer[..length]);
            remaining -= length as u64;
        }
        if input.read(&mut buffer[..1])? != 0 || Stamp::of(&held.metadata()?) != before {
            return Err(changed());
        }
        source.require_child(name, &fstat(&held)?)?;
        fsync(&output)?;
        self.manifest
            .add(relative, Some((before.size, hash.finalize().into())))
    }
}

fn hold(parent: &Directory, name: &str) -> io::Result<File> {
    Ok(File::from(openat(
        &parent.fd,
        name,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?))
}

fn require_login(file: &File, directory: bool, mount: u64, login: (u32, u32)) -> io::Result<()> {
    let stat = fstat(file)?;
    let kind = FileType::from_raw_mode(stat.st_mode);
    let valid = if directory {
        kind == FileType::Directory
    } else {
        kind == FileType::RegularFile && stat.st_nlink == 1
    };
    if !valid
        || (stat.st_uid, stat.st_gid) != login
        || stat.st_mode & 0o7022 != 0
        || mount_id(file)? != mount
    {
        return Err(changed());
    }
    Ok(())
}

fn names(directory: &Directory, limit: usize) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in Dir::read_from(&directory.fd)? {
        let entry = entry?;
        let name = entry.file_name().to_str().map_err(io::Error::other)?;
        if matches!(name, "." | "..") {
            continue;
        }
        if names.len() == limit || !valid_name(name) {
            return Err(changed());
        }
        names.push(name.to_owned());
    }
    names.sort_unstable();
    Ok(names)
}

fn changed() -> io::Error {
    invalid("The private rsync handoff changed or exceeded the approved input inventory")
}
