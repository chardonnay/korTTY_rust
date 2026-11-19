//! Reclaim private rsync stages once their helper no longer holds the marker lock.

use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};

use kortty_server_archive::rsync_tree::valid_stage;
use rustix::fs::{FlockOperation, flock};

use super::super::{draft_replace::mount_id, *};
use super::{Rules, download};

const PENDING: &str = ".kortty-rsync-private-";
const LOCK: &str = ".kortty-rsync-lock";
const UPLOAD: &str = ".upload";
const DOWNLOAD: &str = ".download";
const SWEEP_LIMIT: usize = 1024;
const RECOVERY_LIMIT: usize = 8;
pub(super) const PAYLOAD: &str = "payload";

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Upload,
    Download,
}

impl Kind {
    pub(super) fn marker(self) -> &'static str {
        match self {
            Self::Upload => UPLOAD,
            Self::Download => DOWNLOAD,
        }
    }
}

pub(super) struct Lease {
    pub(super) parent: Directory,
    pub(super) directory: Directory,
    pub(super) name: String,
    pub(super) mount: u64,
    kind: Kind,
    lock: File,
    pending: bool,
}

impl Lease {
    pub(super) fn create(
        rules: &Rules,
        parent: Directory,
        mount: u64,
        kind: Kind,
    ) -> io::Result<Self> {
        sweep(rules, &parent, mount)?;
        Self::allocate(parent, mount, kind)
    }

    fn allocate(parent: Directory, mount: u64, kind: Kind) -> io::Result<Self> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let name = format!(".kortty-{}.kortty-part", hex::encode(random));
        let pending = format!("{PENDING}{name}");
        // A sweep can inspect a pending name only after the creator has sealed its marker.
        let root_lock = lock_root(&parent, mount)?;
        mkdirat(&parent.fd, pending.as_str(), PRIVATE_DIRECTORY)?;
        let directory = parent.child(&pending)?;
        fchmod(&directory.fd, PRIVATE_DIRECTORY)?;
        let lock = open_lock(
            &directory,
            kind.marker(),
            mount,
            OFlags::CREATE | OFlags::EXCL,
        )?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive)?;
        renameat_with(
            &parent.fd,
            &pending,
            &parent.fd,
            &name,
            RenameFlags::NOREPLACE,
        )?;
        drop(root_lock);
        Ok(Self {
            parent,
            directory,
            name,
            mount,
            kind,
            lock,
            pending: false,
        })
    }

    pub(super) fn require_current(&self) -> io::Result<()> {
        self.parent
            .require_child(&self.entry_name(), &self.directory.stat)?;
        require_wrapper(&self.directory, self.mount)
    }

    fn entry_name(&self) -> String {
        if self.pending {
            format!("{PENDING}{}", self.name)
        } else {
            self.name.clone()
        }
    }

    pub(super) fn remove(&mut self, login: (u32, u32)) -> io::Result<()> {
        self.require_current()?;
        self.directory
            .require_child(self.kind.marker(), &fstat(&self.lock)?)?;
        if !self.pending {
            let pending = format!("{PENDING}{}", self.name);
            renameat_with(
                &self.parent.fd,
                &self.name,
                &self.parent.fd,
                &pending,
                RenameFlags::NOREPLACE,
            )?;
            self.pending = true;
        }
        self.require_current()?;
        match self.kind {
            Kind::Upload => discard_upload(&self.directory, self.mount, login)?,
            Kind::Download => download::discard_handoff(&self.directory, self.mount)?,
        }
        fchmod(&self.directory.fd, PRIVATE_DIRECTORY)?;
        let _root_lock = lock_root(&self.parent, self.mount)?;
        self.require_current()?;
        self.directory
            .require_child(self.kind.marker(), &fstat(&self.lock)?)?;
        unlinkat(&self.directory.fd, self.kind.marker(), AtFlags::empty())?;
        unlinkat(&self.parent.fd, self.entry_name(), AtFlags::REMOVEDIR)?;
        Ok(())
    }
}

fn lock_root(root: &Directory, mount: u64) -> io::Result<File> {
    let lock = open_lock(root, LOCK, mount, OFlags::CREATE)?;
    flock(&lock, FlockOperation::LockExclusive)?;
    root.require_child(LOCK, &fstat(&lock)?)?;
    Ok(lock)
}

fn open_lock(directory: &Directory, name: &str, mount: u64, flags: OFlags) -> io::Result<File> {
    let lock = File::from(openat(
        &directory.fd,
        name,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | flags,
        PRIVATE_FILE,
    )?);
    let stat = fstat(&lock)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != 0
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_nlink != 1
        || mount_id(&lock)? != mount
    {
        return Err(stage_changed());
    }
    Ok(lock)
}

fn sweep(rules: &Rules, root: &Directory, mount: u64) -> io::Result<()> {
    let mut recovered = 0;
    for entry in Dir::read_from(&root.fd)?.take(SWEEP_LIMIT) {
        let entry = entry?;
        let Ok(entry) = entry.file_name().to_str() else {
            continue;
        };
        let name = entry.strip_prefix(PENDING).unwrap_or(entry);
        if valid_stage(name) {
            rules.require_current()?;
            let (_, parent, current_mount) = rules.handoff()?;
            if mount != current_mount || !same_node(&root.stat, &parent.stat) {
                return Err(stage_changed());
            }
            // A damaged stage stays private and does not prevent a different transfer.
            if reclaim(parent, entry, name, mount, rules.login()).unwrap_or(false) {
                recovered += 1;
                if recovered == RECOVERY_LIMIT {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn reclaim(
    parent: Directory,
    entry: &str,
    name: &str,
    mount: u64,
    login: (u32, u32),
) -> io::Result<bool> {
    let pending = name != entry;
    let root_lock = lock_root(&parent, mount)?;
    let directory = parent.child(entry)?;
    if pending {
        // A just-created directory can still have the parent's inherited setgid bit.
        let stat = fstat(&directory.fd)?;
        if stat.st_uid != 0 || stat.st_mode & 0o5022 != 0 || mount_id(&directory.fd)? != mount {
            return Err(stage_changed());
        }
    } else {
        require_wrapper(&directory, mount)?;
    }
    let (kind, lock) = match (
        existing_marker(&directory, mount, Kind::Upload)?,
        existing_marker(&directory, mount, Kind::Download)?,
    ) {
        (Some(lock), None) => (Kind::Upload, lock),
        (None, Some(lock)) => (Kind::Download, lock),
        (None, None) if pending => {
            parent.require_child(entry, &directory.stat)?;
            unlinkat(&parent.fd, entry, AtFlags::REMOVEDIR)?;
            return Ok(true);
        }
        _ => return Ok(false),
    };
    match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(Errno::WOULDBLOCK) => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    drop(root_lock);
    if pending && directory.stat.st_mode & 0o2000 != 0 {
        fchmod(&directory.fd, PRIVATE_DIRECTORY)?;
    }
    let mut lease = Lease {
        parent,
        directory,
        name: name.to_owned(),
        mount,
        kind,
        lock,
        pending,
    };
    lease.remove(login)?;
    Ok(true)
}

fn existing_marker(directory: &Directory, mount: u64, kind: Kind) -> io::Result<Option<File>> {
    match open_lock(directory, kind.marker(), mount, OFlags::empty()) {
        Ok(lock) => Ok(Some(lock)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn require_wrapper(directory: &Directory, mount: u64) -> io::Result<()> {
    let stat = fstat(&directory.fd)?;
    if stat.st_uid != 0 || stat.st_mode & 0o7022 != 0 || mount_id(&directory.fd)? != mount {
        return Err(stage_changed());
    }
    Ok(())
}

fn discard_upload(directory: &Directory, mount: u64, (uid, gid): (u32, u32)) -> io::Result<()> {
    let payload = match directory.child(PAYLOAD) {
        Ok(payload) => payload,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if mount_id(&payload.fd)? != mount {
        return Err(stage_changed());
    }
    // A helper can die before chown. Only an empty root-owned setup node can be removed as root.
    if payload.stat.st_uid != 0 {
        if payload.stat.st_uid != uid || payload.stat.st_gid != gid {
            return Err(stage_changed());
        }
        fchmod(&directory.fd, Mode::from_bits_truncate(0o711))?;
        // The login removes its own mutable contents. Root only removes the checked empty wrapper.
        let _ = Command::new("/usr/bin/rm")
            .env_clear()
            .uid(uid)
            .gid(gid)
            .arg("-rf")
            .arg("--one-file-system")
            .arg("--")
            .current_dir(format!("/proc/self/fd/{}", directory.fd.as_raw_fd()))
            .arg(PAYLOAD)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    fchmod(&directory.fd, PRIVATE_DIRECTORY)?;
    directory.require_child(PAYLOAD, &payload.stat)?;
    unlinkat(&directory.fd, PAYLOAD, AtFlags::REMOVEDIR)?;
    Ok(())
}

fn stage_changed() -> io::Error {
    invalid("The private privileged rsync handoff changed or is unavailable")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Child;

    use rustix::fs::fchown;
    use rustix::process::{Gid, Uid};

    use super::*;

    const LOGIN: (u32, u32) = (65534, 65534);
    const CHILD_ROOT: &str = "KORTTY_TEST_RSYNC_ABANDONED_CHILD";

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Helper(Child);

    impl Drop for Helper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "requires Linux root and an ordinary UID/GID"]
    fn reclaims_private_stages_after_helper_is_killed() {
        assert!(geteuid().is_root());
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            staged_child(Path::new(&root));
            return;
        }
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let scratch = Scratch(
            std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("kortty-rsync-recovery-{}", hex::encode(random))),
        );
        std::fs::DirBuilder::new()
            .mode(0o711)
            .create(&scratch.0)
            .unwrap();
        let root = || Directory::open_parent(scratch.0.to_str().unwrap()).unwrap();
        let mount = mount_id(&root().fd).unwrap();
        let test_name = concat!(
            module_path!(),
            "::reclaims_private_stages_after_helper_is_killed"
        );
        let mut helper = Helper(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name.split_once("::").unwrap().1])
                .args(["--ignored", "--nocapture"])
                .env(CHILD_ROOT, &scratch.0)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let output = BufReader::new(helper.0.stdout.take().unwrap());
        let names = output
            .lines()
            .map_while(Result::ok)
            .find_map(|line| {
                let (_, names) = line.split_once("handoff names: ")?;
                let (upload, download) = names.split_once(' ')?;
                Some([upload.to_owned(), download.to_owned()])
            })
            .expect("child allocated both private stages");
        for name in &names {
            assert!(!reclaim(root(), name, name, mount, LOGIN).unwrap());
            assert!(scratch.0.join(name).join(PAYLOAD).exists());
        }
        helper.0.kill().unwrap();
        assert!(!helper.0.wait().unwrap().success());
        for name in &names {
            assert!(reclaim(root(), name, name, mount, LOGIN).unwrap());
            assert!(!scratch.0.join(name).exists());
        }
        let legacy = format!(".kortty-{}.kortty-part", "a".repeat(32));
        mkdirat(&root().fd, &legacy, PRIVATE_DIRECTORY).unwrap();
        assert!(!reclaim(root(), &legacy, &legacy, mount, LOGIN).unwrap());
        assert!(scratch.0.join(&legacy).is_dir());
        let pending = format!("{PENDING}{legacy}");
        mkdirat(&root().fd, &pending, PRIVATE_DIRECTORY).unwrap();
        assert!(reclaim(root(), &pending, &legacy, mount, LOGIN).unwrap());
        assert!(!scratch.0.join(&pending).exists());
    }

    fn staged_child(path: &Path) {
        let root = || Directory::open_parent(path.to_str().unwrap()).unwrap();
        let mount = mount_id(&root().fd).unwrap();
        let upload = Lease::allocate(root(), mount, Kind::Upload).unwrap();
        let download = Lease::allocate(root(), mount, Kind::Download).unwrap();
        mkdirat(&upload.directory.fd, PAYLOAD, PRIVATE_DIRECTORY).unwrap();
        let payload = upload.directory.child(PAYLOAD).unwrap();
        std::fs::write(
            path.join(&upload.name).join(PAYLOAD).join("sample"),
            "upload",
        )
        .unwrap();
        fchown(
            &payload.fd,
            Some(Uid::from_raw(LOGIN.0)),
            Some(Gid::from_raw(LOGIN.1)),
        )
        .unwrap();
        std::fs::write(path.join(&download.name).join(PAYLOAD), "download").unwrap();
        println!("handoff names: {} {}", upload.name, download.name);
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
}
