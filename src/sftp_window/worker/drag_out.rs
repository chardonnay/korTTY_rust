//! Bounded preparation of private files on a separate verified SFTP channel.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use crate::private_files::{Directory, EntryIdentity, nonce};

use super::super::{Entry, EntryKind, EntryName, Location};
use super::{Client, ClipboardPolicy, Control, FileKind, Worker, files};

pub(crate) const MAX_FILES: usize = 20;
pub(crate) const MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const PREPARED_LIFETIME: Duration = Duration::from_secs(60);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_millis(500);

pub(crate) struct Candidate {
    path: Vec<u8>,
    name: String,
    pub(crate) size: u64,
    link: bool,
}

impl Candidate {
    pub(crate) fn for_entry(directory: &Location, entry: &Entry) -> Option<Self> {
        let EntryName::Remote(name) = &entry.name else {
            return None;
        };
        let Location::Remote(path) = directory.with(&entry.name)? else {
            return None;
        };
        Some(Self {
            path,
            name: valid_name(name)?,
            size: entry.size?,
            link: entry.kind == EntryKind::Link,
        })
    }
}

pub(crate) struct Prepared {
    paths: Vec<PathBuf>,
    _staging: Staging,
    connection: Weak<Control>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Prepared desktop drag files")
    }
}

impl Prepared {
    pub(crate) fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub(crate) fn matches(&self, worker: &Worker) -> bool {
        self.connection.ptr_eq(&Arc::downgrade(&worker.control)) && !worker.is_closed()
    }

    pub(crate) fn permitted(&self, policy: &ClipboardPolicy) -> bool {
        self.connection
            .upgrade()
            .is_some_and(|control| control.transfer_allowed(policy) && !control.is_closed())
    }

    pub(crate) fn paths_current(&self) -> bool {
        let stage = &self._staging;
        #[cfg(windows)]
        let open = |path: &Path| Directory::open(path, false);
        #[cfg(target_os = "linux")]
        let open = Directory::open_owner_controlled;
        stage.path.parent().is_some_and(|path| {
            open(path)
                .and_then(|parent| stage.parent.same_as(&parent))
                .unwrap_or(false)
        }) && stage
            .parent
            .private_child(&stage.name, false)
            .and_then(|directory| stage.directory.same_as(&directory))
            .unwrap_or(false)
            && stage
                .names
                .iter()
                .all(|name| stage.directory.open_private_file(name).is_ok())
    }
}

pub(crate) struct Preparation(Arc<AtomicBool>);

impl Drop for Preparation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Worker {
    pub(crate) fn prepare_desktop_drag(
        &self,
        candidates: Vec<Candidate>,
    ) -> io::Result<(Preparation, async_channel::Receiver<io::Result<Prepared>>)> {
        let opener = self
            .transfer_opener()
            .ok_or_else(|| io::Error::other("A verified SFTP connection is required."))?;
        let connection = Arc::downgrade(&self.control);
        let live = Arc::new(AtomicBool::new(true));
        let running = Arc::clone(&live);
        let deadline = Instant::now() + PREPARE_TIMEOUT;
        let (send, receive) = async_channel::bounded(1);
        std::thread::Builder::new()
            .name("kortty-sftp-drag-out".into())
            .spawn(move || {
                let permitted = || {
                    if !running.load(Ordering::Acquire) || send.is_closed() {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "Desktop drag preparation was cancelled.",
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Desktop drag preparation exceeded five seconds.",
                        ));
                    }
                    if connection.upgrade().is_none_or(|control| {
                        control.cancelled.load(Ordering::Acquire)
                            || control.transfer_denied.load(Ordering::Acquire)
                    }) {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "SFTP desktop drag is no longer allowed.",
                        ));
                    }
                    Ok(())
                };
                let result = (|| {
                    permitted()?;
                    if connection.upgrade().is_none_or(|control| {
                        control.is_closed() || !control.transfer_allowed(&ClipboardPolicy::load())
                    }) {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "SFTP desktop drag is no longer allowed.",
                        ));
                    }
                    let mut channel =
                        opener
                            .open_cancellable(|| permitted().is_err())
                            .map_err(|_| {
                                if let Err(error) = permitted() {
                                    return error;
                                }
                                io::Error::other(
                                    "Could not open a verified SFTP channel for desktop drag.",
                                )
                            })?;
                    let reader = channel
                        .take_timed_reader(IO_TIMEOUT)
                        .ok_or_else(|| io::Error::other("SFTP read channel unavailable."))?;
                    let writer = channel
                        .take_timed_writer(IO_TIMEOUT)
                        .ok_or_else(|| io::Error::other("SFTP write channel unavailable."))?;
                    let mut client = Client::new(
                        Box::new(DeadlineIo {
                            io: reader,
                            deadline,
                        }),
                        Box::new(DeadlineIo {
                            io: writer,
                            deadline,
                        }),
                    )
                    .map_err(transfer_error)?;
                    let prepared = prepare(
                        &mut client,
                        candidates,
                        connection.clone(),
                        permitted,
                        Staging::new,
                    )?;
                    if !prepared.permitted(&ClipboardPolicy::load()) {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "SFTP desktop drag is no longer allowed.",
                        ));
                    }
                    Ok(prepared)
                })();
                let _ = send.try_send(result);
            })?;
        Ok((Preparation(live), receive))
    }
}

fn prepare(
    client: &mut Client,
    candidates: Vec<Candidate>,
    connection: Weak<Control>,
    permitted: impl Fn() -> io::Result<()>,
    create_stage: impl FnOnce() -> io::Result<Staging>,
) -> io::Result<Prepared> {
    if candidates.is_empty() || candidates.len() > MAX_FILES {
        return Err(io::Error::other("Select up to 20 regular server files."));
    }
    let mut remaining = MAX_BYTES;
    let mut names = std::collections::HashSet::new();
    let mut checked = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        permitted()?;
        if !names.insert(candidate.name.clone()) {
            return Err(changed());
        }
        let original = client.lstat(&candidate.path).map_err(transfer_error)?;
        if candidate.link != (original.kind() == FileKind::Symlink) {
            return Err(changed());
        }
        let (path, expected) = if candidate.link {
            let Some((target, size)) =
                super::source_link::remote(client, &candidate.path, original)
                    .map_err(|_| changed())?
            else {
                return Err(changed());
            };
            let expected = client.lstat(&target).map_err(transfer_error)?;
            if expected.size != Some(size) {
                return Err(changed());
            }
            (target, expected)
        } else {
            if original.size != Some(candidate.size) {
                return Err(changed());
            }
            (candidate.path, original)
        };
        let size = expected
            .size
            .filter(|&size| size <= remaining && expected.kind() == FileKind::File)
            .ok_or_else(|| {
                io::Error::other("Desktop drag requires regular files totaling at most 16 MiB.")
            })?;
        remaining -= size;
        checked.push((candidate.name, path, expected, size));
    }
    permitted()?;
    let mut staging = create_stage()?;
    let mut paths = Vec::with_capacity(checked.len());
    for (name, path, expected, size) in checked {
        permitted()?;
        staging.names.push(name.clone());
        staging.directory.publish_with(&name, |file| {
            let mut output = Bounded {
                file,
                remaining: size,
                permitted: &permitted,
            };
            let copied = client
                .download_regular(&path, expected, &mut output)
                .map_err(transfer_error)?;
            if copied != size {
                return Err(changed());
            }
            permitted()
        })?;
        paths.push(staging.path.join(&name));
    }
    permitted()?;
    Ok(Prepared {
        paths,
        _staging: staging,
        connection,
    })
}

fn changed() -> io::Error {
    io::Error::other("A selected server file changed or is unavailable.")
}
fn transfer_error(error: kortty_ssh::sftp::Error) -> io::Error {
    match error {
        kortty_ssh::sftp::Error::LocalIo(error) | kortty_ssh::sftp::Error::Transport(error)
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted
                    | io::ErrorKind::TimedOut
                    | io::ErrorKind::PermissionDenied
            ) =>
        {
            error
        }
        _ => changed(),
    }
}

struct DeadlineIo<T> {
    io: T,
    deadline: Instant,
}

impl<T> DeadlineIo<T> {
    fn check(&self) -> io::Result<()> {
        if Instant::now() < self.deadline {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Desktop drag preparation exceeded five seconds.",
            ))
        }
    }
}

impl<T: Read> Read for DeadlineIo<T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.io.read(bytes)
    }
}

impl<T: std::io::Write> std::io::Write for DeadlineIo<T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.io.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check()?;
        self.io.flush()
    }
}

struct Bounded<'a, F> {
    file: &'a mut std::fs::File,
    remaining: u64,
    permitted: &'a F,
}

impl<F: Fn() -> io::Result<()>> io::Write for Bounded<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        (self.permitted)()?;
        if bytes.len() as u64 > self.remaining {
            return Err(changed());
        }
        let count = self.file.write(bytes)?;
        self.remaining -= count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        (self.permitted)()?;
        self.file.flush()
    }
}

struct Staging {
    parent: Directory,
    directory: Directory,
    identity: EntryIdentity,
    name: String,
    path: PathBuf,
    names: Vec<String>,
}

impl Staging {
    #[cfg(target_os = "linux")]
    fn new() -> io::Result<Self> {
        let root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| io::Error::other("A private desktop runtime directory is required."))?;
        Self::in_directory(Directory::open_owner_controlled(&root)?, &root)
    }

    #[cfg(target_os = "windows")]
    fn new() -> io::Result<Self> {
        let root = crate::profile::root()?;
        let parent = Directory::open(&root, false)?.private_child("desktop-drag", true)?;
        Self::in_directory(parent, &root.join("desktop-drag"))
    }

    fn in_directory(parent: Directory, root: &Path) -> io::Result<Self> {
        let name = format!("kortty-sftp-drag-{}", nonce()?);
        let directory = parent.create_private_subdirectory(OsStr::new(&name))?;
        let identity = directory.identity()?;
        let path = root.join(&name);
        Ok(Self {
            parent,
            directory,
            identity,
            name,
            path,
            names: Vec::new(),
        })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = self.directory.remove_file(name);
        }
        let _ = self
            .parent
            .remove_deletion_entry(OsStr::new(&self.name), self.identity, true);
    }
}

pub(crate) fn valid_name(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    files::local_name(name.as_bytes()).ok()?;
    (!name.bytes().any(|byte| byte.is_ascii_control())).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn staging_keeps_private_files_until_the_last_drag_lease_is_released() -> io::Result<()> {
        let scratch = crate::snippets::tests::Profile::new();
        let mut stage = test_stage(&scratch.0.canonicalize()?)?;
        stage.names.push("ü #+.txt".into());
        stage
            .directory
            .publish_copy(&stage.names[0], b"staged bytes")?;
        let path = stage.path.join(&stage.names[0]);
        let lease = Arc::new(Prepared {
            paths: vec![path.clone()],
            _staging: stage,
            connection: Weak::new(),
        });
        let consumer = Arc::clone(&lease);
        drop(lease);
        assert_eq!(std::fs::read(&path)?, b"staged bytes");
        assert!(consumer.paths_current());
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())?
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        drop(consumer);
        assert!(!path.parent().unwrap().exists());
        Ok(())
    }

    fn test_stage(root: &Path) -> io::Result<Staging> {
        #[cfg(unix)]
        let parent = Directory::open_owner_controlled(root)?;
        #[cfg(windows)]
        let parent = Directory::open(root, false)?;
        Staging::in_directory(parent, root)
    }

    #[test]
    fn server_desktop_names_reject_path_components_and_controls() {
        assert_eq!(
            valid_name("ü #+.txt".as_bytes()).as_deref(),
            Some("ü #+.txt")
        );
        for name in [b"".as_slice(), b"..", b"a/b", b"x\ny", b"\xff"] {
            assert!(valid_name(name).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the native OpenSSH sftp-server program"]
    fn openssh_stages_exact_regular_files_and_rejects_oversized_link_targets() -> io::Result<()> {
        use std::os::unix::{ffi::OsStrExt as _, fs::symlink};
        use std::process::{Child, Command, Stdio};

        struct Server(Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize()?;
        let source = root.join("source.txt");
        let alias = root.join("alias.txt");
        let large = root.join("large");
        let oversized_alias = root.join("oversized-alias");
        std::fs::write(&source, b"regular source")?;
        symlink(&source, &alias)?;
        std::fs::File::create(&large)?.set_len(MAX_BYTES + 1)?;
        symlink(&large, &oversized_alias)?;
        let mut server = Server(
            Command::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        );
        let mut client = Client::new(
            Box::new(server.0.stdout.take().unwrap()),
            Box::new(server.0.stdin.take().unwrap()),
        )
        .map_err(transfer_error)?;
        let make = |path: &std::path::Path| -> io::Result<Candidate> {
            let metadata = std::fs::symlink_metadata(path)?;
            Ok(Candidate {
                path: path.as_os_str().as_bytes().to_vec(),
                name: path.file_name().unwrap().to_str().unwrap().to_owned(),
                size: metadata.len(),
                link: metadata.is_symlink(),
            })
        };
        let prepared = prepare(
            &mut client,
            vec![make(&source)?, make(&alias)?],
            Weak::new(),
            || Ok(()),
            || test_stage(&root),
        )?;
        assert_eq!(prepared.paths().len(), 2);
        for file in prepared.paths() {
            assert_eq!(std::fs::read(file)?, b"regular source");
        }
        let stage = prepared.paths()[0].parent().unwrap().to_path_buf();
        drop(prepared);
        assert!(!stage.exists());
        let mut changed_kind = make(&alias)?;
        changed_kind.link = false;
        assert!(
            prepare(
                &mut client,
                vec![changed_kind],
                Weak::new(),
                || Ok(()),
                || panic!("a replaced regular file must be rejected before staging")
            )
            .is_err()
        );
        assert!(
            prepare(
                &mut client,
                vec![make(&oversized_alias)?],
                Weak::new(),
                || Ok(()),
                || panic!("size must be rejected before staging")
            )
            .is_err()
        );
        Ok(())
    }
}
