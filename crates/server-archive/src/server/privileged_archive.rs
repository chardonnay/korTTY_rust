//! Restricts elevated archive creation to administrator-configured Linux paths.

use std::io::{Seek, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kortty_server_archive::protocol::{
    CreateFormat, CreatedPayload, PRIVILEGED_PASSWORD_READY, PRIVILEGED_PATH, PrivilegedReply,
    PrivilegedRequest, read_committed_password, read_privileged_request, require_eof,
    require_privileged_password_commit, write_privileged_reply,
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
    serve_inner(false)
}

pub(crate) fn serve_password() -> io::Result<()> {
    serve_inner(true)
}

fn serve_inner(password_protocol: bool) -> io::Result<()> {
    let mut output = io::stdout().lock();
    let result = (|| {
        require_installation()?;
        // This process serves one connection and never shares its umask.
        umask(Mode::from_bits_truncate(0o077));
        let rules = Rules::load(CONFIG)?;
        let committed = Arc::new(AtomicBool::new(false));
        start_watchdog(Arc::clone(&committed))?;
        if password_protocol {
            output.write_all(PRIVILEGED_PASSWORD_READY)?;
            output.flush()?;
        }
        let publication = transaction(
            io::stdin().lock(),
            &mut output,
            &rules,
            password_protocol,
            || committed.store(true, Ordering::Release),
        )?;
        Ok((rules, publication))
    })();
    match result {
        Ok((rules, Some(publication))) => finish(&mut output, &rules, publication),
        Ok((_, None)) => write_privileged_reply(output, &PrivilegedReply::Aborted),
        Err(error) => write_privileged_reply(output, &failed(error)),
    }
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

pub(super) fn require_installation() -> io::Result<()> {
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
    password_protocol: bool,
    on_commit: impl FnOnce(),
) -> Result<Option<create::Publication>, Failure> {
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
            return Ok(None);
        }
        other => other?,
    };
    let password = match decision {
        PrivilegedRequest::Commit { token: received } if received == token => {
            if spec.format == CreateFormat::ZipPassword {
                Some(read_committed_password(input)?)
            } else {
                if password_protocol {
                    require_privileged_password_commit(input)?;
                } else {
                    require_eof(input)?;
                }
                None
            }
        }
        PrivilegedRequest::Abort => {
            require_eof(input)?;
            return Ok(None);
        }
        _ => {
            return Err(
                invalid("The privileged archive commitment did not match this review").into(),
            );
        }
    };
    rules.require_current()?;
    on_commit();
    let publication = create::apply_with_policy(
        spec,
        &inventory,
        policy,
        password.as_ref().map(|secret| secret.as_slice()),
    )?;
    Ok(Some(publication))
}

fn finish(
    output: &mut impl Write,
    rules: &Rules,
    publication: create::Publication,
) -> io::Result<()> {
    let create::Publication { destination, copy } = publication;
    let Some((payload, mut file)) = copy else {
        return write_privileged_reply(output, &PrivilegedReply::Applied { destination });
    };
    // The path is checked again, but the stream uses only the file held since
    // creation. A revoked rule prevents copying and leaves the archive in place.
    let ready = (|| {
        rules.require_current()?;
        let (parent, name) = absolute_file(&destination)?;
        let parent = rules.require_copy_parent(parent)?;
        let stat = fstat(&file)?;
        require_private_stat(&stat, false)?;
        if Mode::from_raw_mode(stat.st_mode) != PRIVATE_FILE
            || file.metadata()?.len() != payload.bytes
        {
            return Err(invalid("The published privileged archive changed"));
        }
        parent.require_child(name, &stat)?;
        file.rewind()
    })();
    if ready.is_err() {
        return write_privileged_reply(output, &PrivilegedReply::Applied { destination });
    }
    write_privileged_reply(
        &mut *output,
        &PrivilegedReply::AppliedCopy {
            destination,
            payload,
        },
    )?;
    stream_archive_copy(output, rules, &mut file, payload)
}

fn stream_archive_copy(
    output: &mut impl Write,
    rules: &Rules,
    file: &mut File,
    payload: CreatedPayload,
) -> io::Result<()> {
    let mut remaining = payload.bytes;
    let mut buffer = [0; 64 * 1024];
    let mut check = Instant::now();
    while remaining != 0 {
        if check.elapsed() >= Duration::from_millis(250) {
            rules.require_current()?;
            check = Instant::now();
        }
        let limit = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..limit])?;
        output.write_all(&buffer[..limit])?;
        remaining -= limit as u64;
    }
    output.flush()?;
    rules.require_current()
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
    #[serde(default)]
    download_roots: Vec<String>,
}

pub(super) struct Rules {
    configuration: ConfigFile,
    sources: Vec<Root>,
    destinations: Vec<Root>,
    downloads: Vec<Root>,
}

impl Rules {
    fn load(path: &str) -> io::Result<Self> {
        let (configuration, bytes) = ConfigFile::read(path)?;
        let options: Configuration = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("Invalid privileged archive configuration"))?;
        let destinations = Root::open_all(options.destination_roots)?;
        let downloads = if options.download_roots.is_empty() {
            Vec::new()
        } else {
            Root::open_all(options.download_roots)?
        };
        for root in &downloads {
            select_root(&destinations, &root.path)?.open_descendant(&root.path)?;
        }
        Ok(Self {
            configuration,
            sources: Root::open_all(options.source_roots)?,
            destinations,
            downloads,
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

    pub(super) fn require_copy_parent(&self, parent: &str) -> io::Result<Directory> {
        select_root(&self.downloads, parent)
            .map_err(|_| invalid("The administrator has not allowed archive downloads here"))?
            .open_descendant(parent)
    }

    pub(super) fn require_current(&self) -> io::Result<()> {
        let (current, _) = ConfigFile::read(&self.configuration.path)?;
        if current.stamp != self.configuration.stamp || current.digest != self.configuration.digest
        {
            return Err(invalid("The privileged archive configuration changed"));
        }
        for root in self
            .sources
            .iter()
            .chain(&self.destinations)
            .chain(&self.downloads)
        {
            root.open_current()?;
        }
        Ok(())
    }
}

pub(super) struct ConfigFile {
    path: String,
    stamp: Stamp,
    digest: [u8; 32],
}

impl ConfigFile {
    pub(super) fn read(path: &str) -> io::Result<(Self, Vec<u8>)> {
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

    pub(super) fn hash_into(&self, hash: &mut Sha256) {
        self.stamp.hash_into(hash);
        hash.update(self.digest);
    }
}

pub(super) struct Root {
    pub(super) path: String,
    identity: Stat,
    pub(super) mount: u64,
}

impl Root {
    pub(super) fn open_all(paths: Vec<String>) -> io::Result<Vec<Self>> {
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

    pub(super) fn open_current(&self) -> io::Result<Directory> {
        let root = open_protected_directory(&self.path)?;
        if !same_node(&root.stat, &self.identity) || mount_id(&root.fd)? != self.mount {
            return Err(invalid("A configured privileged archive root changed"));
        }
        Ok(root)
    }

    pub(super) fn open_descendant(&self, path: &str) -> io::Result<Directory> {
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

pub(super) fn select_root<'a>(roots: &'a [Root], path: &str) -> io::Result<&'a Root> {
    roots
        .iter()
        .filter(|root| within(&root.path, path))
        .max_by_key(|root| root.path.len())
        .ok_or_else(outside_scope)
}

pub(super) fn within(root: &str, path: &str) -> bool {
    root == path
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn outside_scope() -> io::Error {
    invalid("An archive path is outside the configured privileged archive roots")
}

pub(super) fn open_protected_directory(path: &str) -> io::Result<Directory> {
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
