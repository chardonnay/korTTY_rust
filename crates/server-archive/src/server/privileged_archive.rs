//! Restricts elevated archive creation to administrator-configured Linux paths.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kortty_server_archive::protocol::{
    PRIVILEGED_PATH, PrivilegedReply, PrivilegedRequest, read_privileged_request,
    write_privileged_reply,
};
use rustix::fs::{StatxFlags, fgetxattr_query, fremovexattr, statx};
use serde::Deserialize;

use super::create::{self, PathPolicy};
use super::*;

const CONFIG: &str = "/etc/kortty/privileged-archive.json";
const MAX_CONFIG_BYTES: u64 = 32 * 1024;
const MAX_ROOTS: usize = 16;
const ACL_ACCESS: &str = "system.posix_acl_access";
const ACL_DEFAULT: &str = "system.posix_acl_default";
const COMMIT_TIMEOUT: Duration = Duration::from_secs(300);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(1800);

pub(crate) fn serve() -> io::Result<()> {
    let mut output = io::stdout().lock();
    let result = (|| {
        require_installation()?;
        // This process serves one connection and never shares its umask.
        umask(Mode::from_bits_truncate(0o077));
        let rules = Rules::load(CONFIG)?;
        let committed = Arc::new(AtomicBool::new(false));
        start_watchdog(Arc::clone(&committed))?;
        transaction(io::stdin().lock(), &mut output, &rules, || {
            committed.store(true, Ordering::Release);
        })
    })();
    write_privileged_reply(output, &result.unwrap_or_else(failed))
}

fn start_watchdog(committed: Arc<AtomicBool>) -> io::Result<()> {
    let started = Instant::now();
    std::thread::Builder::new()
        .name("kortty-archive-deadline".into())
        .spawn(move || {
            std::thread::sleep(COMMIT_TIMEOUT);
            if committed.load(Ordering::Acquire) {
                std::thread::sleep(OPERATION_TIMEOUT.saturating_sub(started.elapsed()));
            }
            // A local sudo caller may keep pipes open or never read a reply.
            // Any unpublished stage remains private if the process must exit.
            std::process::exit(124);
        })?;
    Ok(())
}

fn require_installation() -> io::Result<()> {
    if getuid().as_raw() != 0 || geteuid().as_raw() != 0 || getgid() != getegid() {
        return Err(invalid(
            "Privileged archive creation requires the root account",
        ));
    }
    let installed = open_protected_file(PRIVILEGED_PATH)?;
    let actual = std::fs::metadata("/proc/self/exe")?;
    let expected = installed.metadata()?;
    if actual.dev() != expected.dev()
        || actual.ino() != expected.ino()
        || expected.mode() & 0o111 == 0
    {
        return Err(invalid(
            "The privileged archive helper must be installed in the protected location",
        ));
    }
    Ok(())
}

fn transaction(
    mut input: impl Read,
    mut output: impl Write,
    rules: &Rules,
    on_commit: impl FnOnce(),
) -> Result<PrivilegedReply, Failure> {
    let PrivilegedRequest::Review { spec } = read_privileged_request(&mut input)? else {
        return Err(invalid("Privileged archive creation must start with a review").into());
    };
    let policy = PathPolicy::privileged(rules);
    let mut review = create::review_with_policy(spec.clone(), policy)?;
    rules.require_current()?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let token = hex::encode(random);
    let inventory = std::mem::replace(&mut review.token, token.clone());
    write_privileged_reply(&mut output, &PrivilegedReply::Review { review })?;
    let decision = match read_privileged_request(&mut input) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Ok(PrivilegedReply::Aborted);
        }
        other => other?,
    };
    if io::copy(&mut input.take(1), &mut io::sink())? != 0 {
        return Err(invalid("Unexpected input after the privileged archive decision").into());
    }
    match decision {
        PrivilegedRequest::Commit { token: received } if received == token => {
            rules.require_current()?;
            on_commit();
            let destination = create::apply_with_policy(spec, &inventory, policy)?;
            Ok(PrivilegedReply::Applied { destination })
        }
        PrivilegedRequest::Abort => Ok(PrivilegedReply::Aborted),
        _ => Err(invalid("The privileged archive commitment did not match this review").into()),
    }
}

fn failed(failure: Failure) -> PrivilegedReply {
    PrivilegedReply::Failed {
        message: failure.message,
        outcome_unknown: failure.outcome_unknown,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    source_roots: Vec<String>,
    destination_roots: Vec<String>,
}

pub(super) struct Rules {
    configuration: ConfigFile,
    sources: Vec<Root>,
    destinations: Vec<Root>,
}

impl Rules {
    fn load(path: &str) -> io::Result<Self> {
        let (configuration, bytes) = ConfigFile::read(path)?;
        let options: Configuration = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("Invalid privileged archive configuration"))?;
        Ok(Self {
            configuration,
            sources: Root::open_all(options.source_roots)?,
            destinations: Root::open_all(options.destination_roots)?,
        })
    }

    pub(super) fn source_parent(&self, path: &str, parent: &str) -> io::Result<(Directory, u64)> {
        let root = select_root(&self.sources, path)?;
        let directory = if root.path == path {
            // The requested source may be the allowlisted directory itself.
            root.open_current()?;
            open_protected_directory(parent)?
        } else {
            root.open_descendant(parent)?
        };
        Ok((directory, root.mount))
    }

    pub(super) fn destination_parent(&self, parent: &str) -> io::Result<Directory> {
        select_root(&self.destinations, parent)?.open_descendant(parent)
    }

    pub(super) fn require_current(&self) -> io::Result<()> {
        let (current, _) = ConfigFile::read(&self.configuration.path)?;
        if current.stamp != self.configuration.stamp || current.digest != self.configuration.digest
        {
            return Err(invalid("The privileged archive configuration changed"));
        }
        for root in self.sources.iter().chain(&self.destinations) {
            root.open_current()?;
        }
        Ok(())
    }
}

struct ConfigFile {
    path: String,
    stamp: Stamp,
    digest: [u8; 32],
}

impl ConfigFile {
    fn read(path: &str) -> io::Result<(Self, Vec<u8>)> {
        let file = open_protected_file(path)?;
        let stamp = Stamp::of(&file.metadata()?);
        if !(1..=MAX_CONFIG_BYTES).contains(&stamp.size) {
            return Err(invalid(
                "The privileged archive configuration exceeds its size limit or is empty",
            ));
        }
        let mut bytes = Vec::with_capacity(stamp.size as usize);
        (&file).take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != stamp.size
            || stamp != Stamp::of(&file.metadata()?)
            || stamp != Stamp::of(&open_protected_file(path)?.metadata()?)
        {
            return Err(invalid(
                "The privileged archive configuration changed while reading",
            ));
        }
        Ok((
            Self {
                path: path.into(),
                stamp,
                digest: Sha256::digest(&bytes).into(),
            },
            bytes,
        ))
    }
}

struct Root {
    path: String,
    identity: Stat,
    mount: u64,
}

impl Root {
    fn open_all(paths: Vec<String>) -> io::Result<Vec<Self>> {
        if paths.is_empty() || paths.len() > MAX_ROOTS {
            return Err(invalid(
                "Configure between one and sixteen privileged archive roots per role",
            ));
        }
        let mut roots = Vec::<Self>::new();
        for path in paths {
            absolute_file(&path)?;
            if ["/proc", "/sys", "/dev"]
                .iter()
                .any(|root| within(root, &path))
                || roots.iter().any(|root| root.path == path)
            {
                return Err(invalid("Invalid or duplicate privileged archive root"));
            }
            let root = open_protected_directory(&path)?;
            roots.push(Self {
                path,
                identity: root.stat,
                mount: mount_id(&root.fd)?,
            });
        }
        Ok(roots)
    }

    fn open_current(&self) -> io::Result<Directory> {
        let root = open_protected_directory(&self.path)?;
        if !same_node(&root.stat, &self.identity) || mount_id(&root.fd)? != self.mount {
            return Err(invalid("A configured privileged archive root changed"));
        }
        Ok(root)
    }

    fn open_descendant(&self, path: &str) -> io::Result<Directory> {
        let mut directory = self.open_current()?;
        if path == self.path {
            return Ok(directory);
        }
        let relative = path
            .strip_prefix(&self.path)
            .and_then(|path| path.strip_prefix('/'))
            .ok_or_else(outside_scope)?;
        for name in relative.split('/') {
            directory = directory.child(name)?;
            require_source_node(&directory, true, self.mount)?;
        }
        Ok(directory)
    }
}

fn select_root<'a>(roots: &'a [Root], path: &str) -> io::Result<&'a Root> {
    roots
        .iter()
        .filter(|root| within(&root.path, path))
        .max_by_key(|root| root.path.len())
        .ok_or_else(outside_scope)
}

fn within(root: &str, path: &str) -> bool {
    root == path
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn outside_scope() -> io::Error {
    invalid("An archive path is outside the configured privileged archive roots")
}

fn open_protected_directory(path: &str) -> io::Result<Directory> {
    let mut directory = Directory::from_fd(open("/", DIRECTORY, Mode::empty())?)?;
    require_private_node(&directory, true)?;
    if path != "/" {
        absolute_file(path)?;
        for name in path[1..].split('/') {
            directory = directory.child(name)?;
            require_private_node(&directory, true)?;
        }
    }
    Ok(directory)
}

fn open_protected_file(path: &str) -> io::Result<File> {
    let (parent, name) = absolute_file(path)?;
    let parent = open_protected_directory(parent)?;
    let named = statat(&parent.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
    require_private_stat(&named, false)?;
    let opened = Directory::from_fd(openat(&parent.fd, name, SOURCE, Mode::empty())?)?;
    require_private_node(&opened, false)?;
    if !same_node(&named, &opened.stat) {
        return Err(invalid("A protected server file changed while opening"));
    }
    Ok(opened.fd)
}

pub(super) fn require_source_node(node: &Directory, directory: bool, mount: u64) -> io::Result<()> {
    require_private_node(node, directory)?;
    if mount_id(&node.fd)? != mount {
        return Err(invalid(
            "A privileged archive path crosses a mount inside its configured root",
        ));
    }
    Ok(())
}

fn require_private_stat(stat: &Stat, directory: bool) -> io::Result<()> {
    let kind = FileType::from_raw_mode(stat.st_mode);
    let kind_allowed = if directory {
        kind == FileType::Directory
    } else {
        kind == FileType::RegularFile && stat.st_nlink == 1
    };
    if !kind_allowed || stat.st_uid != 0 || stat.st_mode & 0o7022 != 0 {
        return Err(invalid(
            "Privileged archive paths must contain protected root-owned directories and single-link files",
        ));
    }
    Ok(())
}

fn require_private_node(node: &Directory, directory: bool) -> io::Result<()> {
    require_private_stat(&node.stat, directory)?;
    for attribute in [ACL_ACCESS, ACL_DEFAULT] {
        match fgetxattr_query(&node.fd, attribute) {
            Err(Errno::NODATA | Errno::NOTSUP) => {}
            Err(error) => return Err(error.into()),
            Ok(_) => {
                return Err(invalid(
                    "Privileged archive paths cannot use extended POSIX ACLs",
                ));
            }
        }
    }
    Ok(())
}

fn mount_id(file: &File) -> io::Result<u64> {
    let info = statx(file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?;
    if info.stx_mask & StatxFlags::MNT_ID.bits() == 0 {
        return Err(invalid(
            "The Linux server cannot verify privileged archive mount boundaries",
        ));
    }
    Ok(info.stx_mnt_id)
}

pub(super) fn prepare_stage(file: &File) -> io::Result<()> {
    for attribute in [ACL_DEFAULT, ACL_ACCESS] {
        match fremovexattr(file, attribute) {
            Ok(()) | Err(Errno::NODATA | Errno::NOTSUP) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privileged_roots_compare_path_components() {
        assert!(within("/var/kortty/safe", "/var/kortty/safe"));
        assert!(within("/var/kortty/safe", "/var/kortty/safe/folder/a.txt"));
        assert!(!within("/var/kortty/safe", "/var/kortty/safestore/a.txt"));
        assert!(!within("/var/kortty/safe", "/var/kortty"));
        assert!(Root::open_all(vec![]).is_err());
        for invalid_root in [
            "/",
            "/proc",
            "/dev/shm",
            "/sys/fs",
            "/var/../etc",
            "relative",
        ] {
            assert!(Root::open_all(vec![invalid_root.into()]).is_err());
        }
    }
}
