use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kortty_ssh::sftp::{Attributes, Error, FileKind, Sftp};

use super::{Choice, Entry, Layout, Outcome, Progress, check_cancelled, interrupted};
use crate::sftp_window::{staging_name, validate_transfer_name};
use crate::ssh_connection::{Channel, SftpOpener};

#[cfg(all(test, unix))]
mod openssh;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

type Client = Sftp<Box<dyn Read + Send>, Box<dyn Write + Send>>;

pub(super) struct Plan {
    _lease: Channel,
    client: Client,
    root: Vec<u8>,
    directories: HashMap<Vec<u8>, Option<Attributes>>,
    files: HashMap<Vec<u8>, Option<Attributes>>,
    conflicts: Vec<String>,
    overwrite: bool,
}

pub(super) fn prepare(
    opener: SftpOpener,
    cwd: Option<Vec<u8>>,
    layout: &Layout,
    cancelled: &AtomicBool,
) -> io::Result<Plan> {
    check_cancelled(cancelled)?;
    let mut lease = opener
        .open_cancellable(|| cancelled.load(Ordering::Acquire))
        .map_err(|error| {
            if cancelled.load(Ordering::Acquire) {
                interrupted()
            } else {
                io::Error::other(error)
            }
        })?;
    let reader = lease
        .take_timed_reader(REQUEST_TIMEOUT)
        .ok_or_else(channel_closed)?;
    let writer = lease
        .take_timed_writer(REQUEST_TIMEOUT)
        .ok_or_else(channel_closed)?;
    let mut client = Sftp::new(reader, writer).map_err(remote_error)?;
    let mut root = match cwd {
        Some(path) => path,
        None => client.canonicalize(b".").map_err(remote_error)?,
    };
    while root.len() > 1 && root.ends_with(b"/") {
        root.pop();
    }
    if !root.starts_with(b"/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The remote destination is not absolute",
        ));
    }
    let root_attrs = directory(&mut client, &root, None)?;
    let mut plan = Plan {
        _lease: lease,
        client,
        directories: HashMap::from([(root.clone(), Some(root_attrs))]),
        root,
        files: HashMap::new(),
        conflicts: Vec::new(),
        overwrite: true,
    };
    for relative in &layout.directories {
        check_cancelled(cancelled)?;
        let path = destination(&plan.root, relative)?;
        let attrs = plan.inspect(&path)?;
        if let Some(attrs) = attrs {
            directory(&mut plan.client, &path, Some(attrs))?;
        }
        if plan.directories.insert(path, attrs).is_some() {
            return Err(invalid_layout());
        }
    }
    for entry in &layout.entries {
        check_cancelled(cancelled)?;
        let path = destination(&plan.root, &entry.path)?;
        if !matches!(entry.mode, 0o644 | 0o755) || plan.directories.contains_key(&path) {
            return Err(invalid_layout());
        }
        let attrs = plan.inspect(&path)?;
        if let Some(attrs) = attrs {
            plan.conflicts.push(entry.path.clone());
            plan.overwrite &= replaceable(attrs);
        }
        if plan.files.insert(path, attrs).is_some() {
            return Err(invalid_layout());
        }
    }
    plan.overwrite &= plan.client.supports_atomic_replace() && !plan.conflicts.is_empty();
    Ok(plan)
}

impl Plan {
    pub(super) fn target(&self) -> String {
        display_path(&self.root)
    }

    pub(super) fn conflicts(&self) -> &[String] {
        &self.conflicts
    }

    pub(super) fn can_overwrite(&self) -> bool {
        self.overwrite
    }

    fn inspect(&mut self, path: &[u8]) -> io::Result<Option<Attributes>> {
        let parent = parent(path);
        let expected = *self.directories.get(parent).ok_or_else(invalid_layout)?;
        if let Some(attrs) = expected {
            directory(&mut self.client, parent, Some(attrs))?;
            lstat(&mut self.client, path)
        } else {
            Ok(None)
        }
    }

    pub(super) fn run(
        mut self,
        layout: &Layout,
        choice: Choice,
        cancelled: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> io::Result<Outcome> {
        if layout.directories.len() + 1 != self.directories.len()
            || layout.entries.len() != self.files.len()
        {
            return Err(invalid_layout());
        }
        match choice {
            Choice::Copy if !self.conflicts.is_empty() => return Err(destination_changed()),
            Choice::Overwrite if !self.overwrite => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "The server cannot safely replace every conflicting destination",
                ));
            }
            _ => {}
        }
        let mut outcome = Outcome::default();
        let result = self.copy(layout, choice, cancelled, progress, &mut outcome);
        match result {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => outcome.cancelled = true,
            result => result?,
        }
        Ok(outcome)
    }

    fn copy(
        &mut self,
        layout: &Layout,
        choice: Choice,
        cancelled: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
        outcome: &mut Outcome,
    ) -> io::Result<()> {
        let root_attrs = self.directories[&self.root];
        directory(&mut self.client, &self.root, root_attrs)?;
        for relative in &layout.directories {
            check_cancelled(cancelled)?;
            let path = destination(&self.root, relative)?;
            self.verify_parent(&path)?;
            let expected = *self.directories.get(&path).ok_or_else(invalid_layout)?;
            let attrs = match expected {
                Some(attrs) => directory(&mut self.client, &path, Some(attrs))?,
                None => self.create_directory(&path)?,
            };
            self.directories.insert(path, Some(attrs));
        }
        let total = layout.entries.len();
        for (done, entry) in layout.entries.iter().enumerate() {
            check_cancelled(cancelled)?;
            progress(Progress {
                done,
                total,
                path: entry.path.clone(),
            });
            let path = destination(&self.root, &entry.path)?;
            let expected = *self.files.get(&path).ok_or_else(invalid_layout)?;
            self.verify_parent(&path)?;
            let current = lstat(&mut self.client, &path)?;
            let published = if may_publish(choice, expected, current)? {
                self.upload(entry, &path, expected, choice, cancelled)?
            } else {
                false
            };
            if published {
                outcome.written += 1;
            } else {
                outcome.skipped += 1;
            }
        }
        progress(Progress {
            done: total,
            total,
            path: String::new(),
        });
        Ok(())
    }

    fn verify_parent(&mut self, path: &[u8]) -> io::Result<()> {
        let parent = parent(path);
        let expected = self
            .directories
            .get(parent)
            .copied()
            .flatten()
            .ok_or_else(invalid_layout)?;
        directory(&mut self.client, parent, Some(expected))?;
        Ok(())
    }

    fn create_directory(&mut self, path: &[u8]) -> io::Result<Attributes> {
        self.client
            .mkdir_with_permissions(path, 0o755)
            .map_err(remote_error)?;
        let attrs = directory(&mut self.client, path, None)?;
        if !mode(attrs).is_some_and(usable_directory_mode) {
            return Err(io::Error::other(format!(
                "The server created a directory with unexpected permissions: {}",
                display_path(path),
            )));
        }
        Ok(attrs)
    }

    fn upload(
        &mut self,
        entry: &Entry,
        path: &[u8],
        expected: Option<Attributes>,
        choice: Choice,
        cancelled: &AtomicBool,
    ) -> io::Result<bool> {
        check_cancelled(cancelled)?;
        let stage = join(parent(path), staging_name()?.as_bytes());
        let payload = join(&stage, b"payload");
        self.client.mkdir_private(&stage).map_err(|error| {
            if error.connection_lost() {
                cleanup_error(&stage, remote_error(error))
            } else {
                remote_error(error)
            }
        })?;
        let attrs = private_directory(&mut self.client, &stage, None)
            .map_err(|error| cleanup_error(&stage, error))?;
        let result = (|| {
            let file = self
                .client
                .upload_new_with_permissions(
                    &payload,
                    CancellableSource {
                        bytes: entry.content.as_bytes(),
                        cancelled,
                    },
                    entry.mode,
                )
                .map_err(|error| {
                    if matches!(error, Error::LocalIo(_)) && cancelled.load(Ordering::Acquire) {
                        interrupted()
                    } else {
                        remote_error(error)
                    }
                })?;
            check_cancelled(cancelled)?;
            private_directory(&mut self.client, &stage, Some(attrs))?;
            let current = self.client.lstat(&payload).map_err(remote_error)?;
            if !same_file(file, current)
                || file.size != Some(entry.content.len() as u64)
                || !same_owner(file, attrs)
                || expected.is_some_and(|target| !same_owner(target, file))
            {
                return Err(io::Error::other(
                    "Could not verify the staged remote snippet",
                ));
            }
            self.verify_parent(path)?;
            private_directory(&mut self.client, &stage, Some(attrs))?;
            let current = lstat(&mut self.client, path)?;
            if !may_publish(choice, expected, current)? {
                return Ok(false);
            }
            check_cancelled(cancelled)?;
            let published = if expected.is_some() {
                self.client.replace(&payload, path)
            } else if self.client.supports_hardlink() {
                self.client.hardlink(&payload, path)
            } else {
                self.client.rename(&payload, path)
            };
            match published {
                Ok(()) => Ok(true),
                Err(error) if error.connection_lost() => Err(io::Error::other(format!(
                    "Could not confirm publication of {} ({error})",
                    display_path(path),
                ))),
                Err(error) if matches!(choice, Choice::Skip) => {
                    self.verify_parent(path)?;
                    if lstat(&mut self.client, path)?.is_some() {
                        Ok(false)
                    } else {
                        Err(remote_error(error))
                    }
                }
                Err(error) => Err(remote_error(error)),
            }
        })();
        let cleanup = (|| {
            private_directory(&mut self.client, &stage, Some(attrs))?;
            if let Some(file) = lstat(&mut self.client, &payload)? {
                if file.kind() != FileKind::File || !same_owner(file, attrs) {
                    return Err(destination_changed());
                }
                self.client.remove_file(&payload).map_err(remote_error)?;
            }
            self.client.remove_dir(&stage).map_err(remote_error)
        })();
        match (result, cleanup) {
            (result, Ok(())) => result,
            (Ok(_), Err(cleanup)) => Err(cleanup_error(&stage, cleanup)),
            (Err(error), Err(cleanup)) => Err(io::Error::other(format!(
                "{error}. {}",
                cleanup_error(&stage, cleanup),
            ))),
        }
    }
}

fn destination(root: &[u8], relative: &str) -> io::Result<Vec<u8>> {
    for part in relative.as_bytes().split(|byte| *byte == b'/') {
        validate_transfer_name(part)?;
    }
    Ok(join(root, relative.as_bytes()))
}

fn join(directory: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = directory.to_vec();
    if !path.ends_with(b"/") {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}

fn parent(path: &[u8]) -> &[u8] {
    let slash = path.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
    &path[..slash.max(1)]
}

fn lstat(client: &mut Client, path: &[u8]) -> io::Result<Option<Attributes>> {
    match client.lstat(path) {
        Ok(attrs) => Ok(Some(attrs)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(remote_error(error)),
    }
}

fn directory(
    client: &mut Client,
    path: &[u8],
    expected: Option<Attributes>,
) -> io::Result<Attributes> {
    let attrs = client.lstat(path).map_err(remote_error)?;
    if attrs.kind() != FileKind::Directory
        || expected.is_some_and(|expected| {
            attrs.permissions != expected.permissions
                || attrs.uid != expected.uid
                || attrs.gid != expected.gid
        })
        || client.canonicalize(path).map_err(remote_error)? != path
    {
        return Err(io::Error::other(format!(
            "Remote destination is unsafe or changed: {}",
            display_path(path),
        )));
    }
    Ok(attrs)
}

fn private_directory(
    client: &mut Client,
    path: &[u8],
    expected: Option<Attributes>,
) -> io::Result<Attributes> {
    let attrs = directory(client, path, expected)?;
    if mode(attrs) != Some(0o700) || attrs.uid.is_none() || attrs.gid.is_none() {
        return Err(io::Error::other(format!(
            "Could not verify a private remote staging directory: {}",
            display_path(path),
        )));
    }
    Ok(attrs)
}

fn mode(attrs: Attributes) -> Option<u32> {
    attrs.permissions.map(|bits| bits & 0o7777)
}

fn usable_directory_mode(mode: u32) -> bool {
    mode & 0o700 == 0o700 && mode & !0o755 == 0
}

fn same_owner(first: Attributes, second: Attributes) -> bool {
    first.uid.is_some() && first.gid.is_some() && first.uid == second.uid && first.gid == second.gid
}

fn same_file(mut first: Attributes, mut second: Attributes) -> bool {
    first.accessed = None;
    second.accessed = None;
    first == second
}

fn replaceable(attrs: Attributes) -> bool {
    attrs.kind() == FileKind::File
        && attrs.uid.is_some()
        && attrs.gid.is_some()
        && attrs.size.is_some()
        && attrs.modified.is_some()
}

fn may_publish(
    choice: Choice,
    expected: Option<Attributes>,
    current: Option<Attributes>,
) -> io::Result<bool> {
    match (choice, expected, current) {
        (Choice::Skip, Some(_), _) | (Choice::Skip, _, Some(_)) => Ok(false),
        (_, None, None) => Ok(true),
        (Choice::Overwrite, Some(expected), Some(current))
            if replaceable(current) && same_file(expected, current) =>
        {
            Ok(true)
        }
        _ => Err(destination_changed()),
    }
}

fn display_path(path: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut shown = String::new();
    for chunk in path.utf8_chunks() {
        for character in chunk.valid().chars() {
            if character.is_control()
                || matches!(character, '\\' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                shown.extend(character.escape_default());
            } else {
                shown.push(character);
            }
        }
        for byte in chunk.invalid() {
            let _ = write!(shown, "\\x{byte:02x}");
        }
    }
    shown
}

fn destination_changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "The remote destination changed. Review the conflicts again.",
    )
}

fn invalid_layout() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Invalid remote snippet layout")
}

fn channel_closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "The terminal SFTP channel closed",
    )
}

fn remote_error(error: Error) -> io::Error {
    io::Error::other(error)
}

fn cleanup_error(stage: &[u8], error: io::Error) -> io::Error {
    io::Error::other(format!(
        "Could not clean up remote staging directory {} ({error})",
        display_path(stage),
    ))
}

struct CancellableSource<'a> {
    bytes: &'a [u8],
    cancelled: &'a AtomicBool,
}

impl Read for CancellableSource<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) {
            // SFTP retries `Interrupted` reads, so use a distinct local error.
            return Err(io::Error::other("Snippet transfer cancelled"));
        }
        self.bytes.read(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_destination_preserves_native_path_and_rejects_ambiguous_components() {
        assert_eq!(destination(b"/x/\xff", "a/b").unwrap(), b"/x/\xff/a/b");
        assert_eq!(destination(b"/", "a").unwrap(), b"/a");
        assert_eq!(display_path(b"/x/\xff/\n"), "/x/\\xff/\\n");
        for path in ["", "/abs", "../out", "a/./b", "a//b", "a\\b", "a:b", "a\0b"] {
            assert!(destination(b"/home/user", path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn replacement_requires_original_metadata_and_a_regular_file() {
        let expected = Attributes {
            size: Some(30),
            uid: Some(1000),
            gid: Some(1000),
            permissions: Some(0o100644),
            accessed: Some(100),
            modified: Some(100),
        };
        let read = Attributes {
            accessed: Some(101),
            ..expected
        };
        let edited = Attributes {
            modified: Some(101),
            ..expected
        };
        let link = Attributes {
            permissions: Some(0o120777),
            ..expected
        };
        assert!(may_publish(Choice::Overwrite, Some(expected), Some(read)).unwrap());
        assert!(may_publish(Choice::Overwrite, Some(expected), Some(edited)).is_err());
        assert!(may_publish(Choice::Overwrite, Some(link), Some(link)).is_err());
        assert!(may_publish(Choice::Overwrite, None, Some(expected)).is_err());
        assert!(may_publish(Choice::Copy, None, Some(expected)).is_err());
        assert!(!may_publish(Choice::Skip, None, Some(link)).unwrap());
        assert!(!may_publish(Choice::Skip, Some(link), None).unwrap());
        assert!(may_publish(Choice::Copy, None, None).unwrap());
    }
}
