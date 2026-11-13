use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kortty_recording::FILE_EXTENSION;

const LIST_LIMIT: usize = 4096;

#[derive(Clone, Debug)]
pub struct Entry {
    pub(super) path: PathBuf,
    stamp: Stamp,
}

impl Entry {
    pub(super) fn manage(
        &self,
        directory: &Path,
        metadata: &Metadata,
        complete: bool,
    ) -> Result<ManagedFile, String> {
        if !complete || !recording_path(&self.path) || self.path.parent() != Some(directory) {
            return Err("Only complete recordings in the managed folder can be changed.".into());
        }
        if self.stamp != Stamp::of(metadata) {
            return Err("The recording changed. Refresh the list before changing it.".into());
        }
        Ok(ManagedFile {
            path: self.path.clone(),
            stamp: self.stamp.clone(),
        })
    }
}

#[derive(Clone, Debug)]
pub(super) struct ManagedFile {
    path: PathBuf,
    stamp: Stamp,
}

impl ManagedFile {
    pub(super) fn change(self, operation: Operation) -> io::Result<Change> {
        let parent = self.path.parent().ok_or_else(invalid_name)?;
        let old_name = self.path.file_name().ok_or_else(invalid_name)?;
        let destination = match operation {
            Operation::Rename(name) => Some(checked_name(&name)?),
            Operation::Delete => None,
        };
        let source = Source::open(parent, old_name)?;
        if self.stamp != Stamp::of(&source.file.metadata()?) {
            return Err(io::Error::other(
                "Recording changed since it was opened. Open it again before changing it.",
            ));
        }
        let renamed = if let Some(name) = destination {
            let path = parent.join(&name);
            if path != self.path {
                source.rename(parent, old_name, OsStr::new(&name))?;
            }
            Some(path)
        } else {
            source.delete(parent, old_name)?;
            None
        };
        Ok(Change {
            original: self.path,
            renamed,
        })
    }
}

pub(super) enum Operation {
    Rename(String),
    Delete,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub(super) original: PathBuf,
    pub(super) renamed: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    #[cfg(windows)]
    attributes: (u64, u32),
}

impl Stamp {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt as _;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
            #[cfg(windows)]
            attributes: (metadata.creation_time(), metadata.file_attributes()),
        }
    }
}

struct Source {
    file: File,
    #[cfg(unix)]
    directory: File,
}

impl Source {
    fn open(parent: &Path, name: &OsStr) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, open, openat};
            let directory = File::from(open(
                parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            let file = regular(File::from(openat(&directory, name, flags, Mode::empty())?))?;
            Ok(Self { file, directory })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                file: open_regular(&parent.join(name))?,
            })
        }
    }

    fn rename(&self, _parent: &Path, from: &OsStr, to: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            use rustix::fs::{RenameFlags, renameat_with};
            Ok(renameat_with(
                &self.directory,
                from,
                &self.directory,
                to,
                RenameFlags::NOREPLACE,
            )?)
        }
        #[cfg(windows)]
        {
            kortty_files::rename_no_replace(&_parent.join(from), &_parent.join(to))
        }
    }

    fn delete(&self, _parent: &Path, name: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, unlinkat};
            Ok(unlinkat(&self.directory, name, AtFlags::empty())?)
        }
        #[cfg(windows)]
        {
            fs::remove_file(_parent.join(name))
        }
    }
}

pub(super) fn open_regular(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags, open};
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        regular(File::from(open(path, flags, Mode::empty())?))
    }
    #[cfg(windows)]
    {
        kortty_files::open_regular_without_reparse(path)
    }
}

#[cfg(unix)]
fn regular(file: File) -> io::Result<File> {
    if file.metadata()?.is_file() {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Recording must be a regular file",
        ))
    }
}

pub(super) fn list(directory: &Path) -> io::Result<Vec<Entry>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index == LIST_LIMIT {
            return Err(io::Error::other(
                "The recording directory exceeds 4096 entries",
            ));
        }
        let entry = entry?;
        let path = entry.path();
        if !recording_path(&path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        paths.push((
            modified,
            Entry {
                path,
                stamp: Stamp::of(&metadata),
            },
        ));
    }
    paths.sort_unstable_by(|(left_time, left), (right_time, right)| {
        (right_time, &right.path).cmp(&(left_time, &left.path))
    });
    Ok(paths.into_iter().map(|(_, entry)| entry).collect())
}

fn recording_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(FILE_EXTENSION))
}

pub(super) fn checked_name(input: &str) -> io::Result<String> {
    let input = input.trim();
    let name = match input.rsplit_once('.') {
        Some((base, extension)) if extension.eq_ignore_ascii_case(FILE_EXTENSION) => base,
        _ => input,
    };
    if name.is_empty()
        || name.len() > 128
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|ch| ch.is_control() || "<>:\"/\\|?*".contains(ch))
    {
        return Err(invalid_name());
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|tail| {
            matches!(
                tail,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    }) {
        return Err(invalid_name());
    }
    Ok(format!("{name}.{FILE_EXTENSION}"))
}

fn invalid_name() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Enter a single valid recording name",
    )
}
