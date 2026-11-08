use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sftp_window::{
    LocalFileStamp, Location, local_tree, staging_name, validate_transfer_name,
};
use local_tree::{Directory, Entry as LocalEntry};

use super::{Choice, Entry, Layout, Outcome, Progress};

#[cfg(unix)]
type DirectoryIdentity = (u64, u64);
#[cfg(windows)]
type DirectoryIdentity = (u64, [u8; 16]);

const PAYLOAD: &str = "payload";

enum Collision {
    Regular(LocalFileStamp),
    Other,
}

pub(super) struct Plan {
    root: PathBuf,
    directory: Directory,
    directories: HashMap<String, Option<DirectoryIdentity>>,
    reviewed: HashMap<String, Collision>,
    conflicts: Vec<String>,
}

pub(super) fn prepare(root: PathBuf, layout: &Layout, cancelled: &AtomicBool) -> io::Result<Plan> {
    check_cancelled(cancelled)?;
    let directory = Directory::open_path(&root)?;
    let mut plan = Plan {
        root,
        directory,
        directories: HashMap::with_capacity(layout.directories.len()),
        reviewed: HashMap::new(),
        conflicts: Vec::new(),
    };

    // The layout lists directories before their children, including empty folders.
    for path in &layout.directories {
        check_cancelled(cancelled)?;
        let (parent, name) = split(path)?;
        plan.check_parent(parent)?;
        let identity = match plan.open_directory(parent)? {
            Some(parent) => match lookup(&parent, name)? {
                Some(LocalEntry::Directory(directory)) => Some(directory.identity()?),
                Some(_) => return Err(blocked_directory(path)),
                None => None,
            },
            None => None,
        };
        if plan.directories.insert(path.clone(), identity).is_some() {
            return Err(invalid_layout());
        }
    }
    let mut seen = HashSet::with_capacity(layout.entries.len());
    for entry in &layout.entries {
        check_cancelled(cancelled)?;
        let (parent, name) = split(&entry.path)?;
        plan.check_parent(parent)?;
        if !seen.insert(entry.path.as_str()) || plan.directories.contains_key(&entry.path) {
            return Err(invalid_layout());
        }
        if let Some(parent) = plan.open_directory(parent)?
            && let Some(existing) = lookup(&parent, name)?
        {
            plan.conflicts.push(entry.path.clone());
            let collision = match existing {
                LocalEntry::File(file) if reviewable(&file)? => {
                    Collision::Regular(LocalFileStamp::capture(&file.metadata()?))
                }
                _ => Collision::Other,
            };
            plan.reviewed.insert(entry.path.clone(), collision);
        }
    }
    Ok(plan)
}

impl Plan {
    pub(super) fn target(&self) -> String {
        Location::Local(self.root.clone()).display()
    }

    pub(super) fn conflicts(&self) -> &[String] {
        &self.conflicts
    }

    pub(super) fn can_overwrite(&self) -> bool {
        cfg!(unix)
            && self
                .reviewed
                .values()
                .all(|collision| matches!(collision, Collision::Regular(_)))
    }

    pub(super) fn run(
        mut self,
        layout: &Layout,
        choice: Choice,
        cancelled: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
    ) -> io::Result<Outcome> {
        let mut outcome = Outcome::default();
        match self.execute(layout, choice, cancelled, progress, &mut outcome) {
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted
                    && cancelled.load(Ordering::Relaxed) =>
            {
                outcome.cancelled = true;
            }
            result => result?,
        }
        Ok(outcome)
    }

    fn execute(
        &mut self,
        layout: &Layout,
        choice: Choice,
        cancelled: &AtomicBool,
        progress: &mut dyn FnMut(Progress),
        outcome: &mut Outcome,
    ) -> io::Result<()> {
        check_cancelled(cancelled)?;
        match choice {
            Choice::Copy if !self.conflicts.is_empty() => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "The local transfer has existing destinations",
                ));
            }
            Choice::Overwrite if !self.can_overwrite() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "One or more local destinations cannot be safely overwritten",
                ));
            }
            _ => {}
        }
        for path in &layout.directories {
            check_cancelled(cancelled)?;
            if self.open_directory(path)?.is_none() {
                let (parent, name) = split(path)?;
                let parent = self.required_directory(parent)?;
                let child = parent.create_child(name)?;
                #[cfg(unix)]
                child
                    .file()
                    .set_permissions(std::fs::Permissions::from_mode(0o755))?;
                self.directories
                    .insert(path.clone(), Some(child.identity()?));
            }
        }

        let total = layout.entries.len();
        for (done, entry) in layout.entries.iter().enumerate() {
            check_cancelled(cancelled)?;
            progress(Progress {
                done,
                total,
                path: entry.path.clone(),
            });
            if self.write(entry, &choice, cancelled)? {
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

    fn write(&self, entry: &Entry, choice: &Choice, cancelled: &AtomicBool) -> io::Result<bool> {
        check_cancelled(cancelled)?;
        let (path, name) = split(&entry.path)?;
        let parent = self.required_directory(path)?;
        let existing = lookup(&parent, name)?;
        let expected = match (choice, self.reviewed.get(&entry.path), existing) {
            (Choice::Skip, _, Some(_)) => return Ok(false),
            (Choice::Overwrite, Some(Collision::Regular(stamp)), Some(LocalEntry::File(file)))
                if LocalFileStamp::capture(&file.metadata()?) == *stamp && reviewable(&file)? =>
            {
                Some((stamp, file))
            }
            (_, None, None) | (Choice::Skip, _, None) => None,
            _ => return Err(changed_destination(&entry.path)),
        };

        let stage = Stage::create(&parent)?;
        let file = stage.file()?;
        write_payload(file, entry, cancelled)?;
        if let Some((_, existing)) = &expected {
            verify_replacement(file, existing)?;
        }
        file.sync_all()?;
        if !parent.same_as(&self.required_directory(path)?)? {
            return Err(blocked_directory(path));
        }
        check_cancelled(cancelled)?;
        match stage.publish(name, expected.as_ref().map(|(stamp, _)| *stamp)) {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(choice, Choice::Skip)
                    && error.kind() == io::ErrorKind::AlreadyExists =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn open_directory(&self, path: &str) -> io::Result<Option<Directory>> {
        let mut current = Directory::open_path(&self.root)?;
        if !current.same_as(&self.directory)? {
            return Err(blocked_directory(&self.root.display().to_string()));
        }
        if path.is_empty() {
            return Ok(Some(current));
        }
        let mut relative = String::new();
        for name in path.split('/') {
            validate_transfer_name(name.as_bytes())?;
            if !relative.is_empty() {
                relative.push('/');
            }
            relative.push_str(name);
            let expected = self.directories.get(&relative).ok_or_else(invalid_layout)?;
            match (expected, lookup(&current, OsStr::new(name))?) {
                (None, None) => return Ok(None),
                (Some(expected), Some(LocalEntry::Directory(directory)))
                    if directory.identity()? == *expected =>
                {
                    current = directory;
                }
                _ => return Err(blocked_directory(&relative)),
            }
        }
        Ok(Some(current))
    }

    fn required_directory(&self, path: &str) -> io::Result<Directory> {
        self.open_directory(path)?
            .ok_or_else(|| blocked_directory(path))
    }

    fn check_parent(&self, path: &str) -> io::Result<()> {
        if path.is_empty() || self.directories.contains_key(path) {
            Ok(())
        } else {
            Err(invalid_layout())
        }
    }
}

struct Stage<'a> {
    parent: &'a Directory,
    name: OsString,
    directory: Option<Directory>,
    file: Option<File>,
}

impl<'a> Stage<'a> {
    fn create(parent: &'a Directory) -> io::Result<Self> {
        let name = OsString::from(staging_name()?);
        let directory = parent.create_child(&name)?;
        let file = match directory.create_file(OsStr::new(PAYLOAD)) {
            Ok(file) => file,
            Err(error) => {
                let _ = local_tree::discard(parent, &name, directory);
                return Err(error);
            }
        };
        let stage = Self {
            parent,
            name,
            directory: Some(directory),
            file: Some(file),
        };
        #[cfg(unix)]
        {
            require_no_extended_acl(stage.directory()?.file())?;
            require_no_extended_acl(stage.file()?)?;
        }
        Ok(stage)
    }

    fn directory(&self) -> io::Result<&Directory> {
        self.directory
            .as_ref()
            .ok_or_else(|| io::Error::other("The local staging directory is unavailable"))
    }

    fn file(&self) -> io::Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| io::Error::other("The staged local file is unavailable"))
    }

    fn publish(&self, name: &OsStr, expected: Option<&LocalFileStamp>) -> io::Result<()> {
        let directory = self.directory()?;
        let file = self.file()?;
        let payload = OsStr::new(PAYLOAD);
        if let Some(expected) = expected {
            #[cfg(unix)]
            return self
                .parent
                .replace_file(directory, payload, file, name, expected);
            #[cfg(windows)]
            {
                let _ = expected;
                return Err(io::ErrorKind::Unsupported.into());
            }
        }
        local_tree::publish_file(self.parent, directory, payload, file, name)
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Some(directory) = self.directory.take() {
            let _ = local_tree::discard(self.parent, &self.name, directory);
        }
    }
}

fn write_payload(mut file: &File, entry: &Entry, cancelled: &AtomicBool) -> io::Result<()> {
    for bytes in entry.content.as_bytes().chunks(64 * 1024) {
        check_cancelled(cancelled)?;
        file.write_all(bytes)?;
    }
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(entry.mode & 0o777))?;
    Ok(())
}

fn lookup(parent: &Directory, name: &OsStr) -> io::Result<Option<LocalEntry>> {
    match local_tree::entry(parent, name) {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn reviewable(file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let metadata = file.metadata()?;
        Ok(metadata.nlink() == 1
            && metadata.mode() & 0o7000 == 0
            && require_no_extended_acl(file).is_ok())
    }
    #[cfg(windows)]
    {
        let _ = file;
        Ok(false)
    }
}

fn verify_replacement(staged: &File, existing: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        let staged = staged.metadata()?;
        let existing = existing.metadata()?;
        if staged.uid() == existing.uid() && staged.gid() == existing.gid() {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The replacement would change the destination owner or group",
        ))
    }
    #[cfg(windows)]
    {
        let _ = (staged, existing);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(target_os = "macos")]
use kortty_files::require_no_extended_acl;

#[cfg(target_os = "linux")]
fn require_no_extended_acl(file: &File) -> io::Result<()> {
    use rustix::io::Errno;
    match rustix::fs::fgetxattr_query(file, "system.posix_acl_access") {
        Err(Errno::NODATA | Errno::NOTSUP) => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The local transfer cannot preserve an extended ACL",
        )),
    }
}

fn split(path: &str) -> io::Result<(&str, &OsStr)> {
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
    if path.starts_with('/') {
        return Err(invalid_layout());
    }
    validate_transfer_name(name.as_bytes())?;
    Ok((parent, OsStr::new(name)))
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(())
}

fn invalid_layout() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "Invalid local transfer layout")
}

fn blocked_directory(path: &str) -> io::Error {
    io::Error::other(format!(
        "The local transfer folder is blocked or changed: {path}"
    ))
}

fn changed_destination(path: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("The local transfer destination changed: {path}"),
    )
}

#[cfg(test)]
mod tests;
