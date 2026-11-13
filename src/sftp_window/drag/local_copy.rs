//! Desktop drops onto the local pane, with no SSH operations.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_channel::Receiver;
use iced::Task;

use super::super::files::{self, LocalStamp};
use super::super::local_tree::{self, Directory, Entry};
use super::super::tree::{MAX_BYTES, MAX_DEPTH, MAX_ENTRIES, ordinary_download::Stage};
use super::super::worker::resume::persist::Identity;
use super::super::{Message, OrdinaryDownload, SftpWindow};

#[cfg(test)]
mod tests;

pub(super) struct Prepared {
    target: PathBuf,
    parent: Identity,
    sources: Vec<PathBuf>,
}

impl Prepared {
    pub(super) fn new(target: PathBuf, sources: Vec<PathBuf>) -> io::Result<Self> {
        let parent = Identity::directory(&Directory::open_path(&target)?)?;
        Ok(Self {
            target,
            parent,
            sources,
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn from_wayland(
        target: PathBuf,
        mut sources: Vec<PathBuf>,
    ) -> io::Result<(Self, usize)> {
        let count = sources.len();
        sources.retain(|path| {
            let Some((parent, name)) = path.parent().zip(path.file_name()) else {
                return false;
            };
            Directory::open_path(parent)
                .and_then(|directory| local_tree::entry(&directory, name))
                .is_ok_and(|entry| !matches!(entry, Entry::Skip))
        });
        let rejected = count - sources.len();
        if sources.is_empty() {
            return Err(io::Error::other(
                "Drop accessible regular files or folders.",
            ));
        }
        Self::new(target, sources).map(|prepared| (prepared, rejected))
    }
}

#[derive(Clone)]
pub struct Reply {
    ticket: Arc<()>,
    status: Arc<str>,
    listing: Option<Arc<super::super::Directory>>,
}

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SFTP local copy completed")
    }
}

pub(in crate::sftp_window) struct Job {
    ticket: Arc<()>,
    cancel: Arc<AtomicBool>,
    done: Receiver<()>,
}

impl Job {
    pub(in crate::sftp_window) fn stop(&self) -> Receiver<()> {
        self.cancel.store(true, Ordering::Release);
        self.done.clone()
    }

    pub(in crate::sftp_window) fn stopping(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl SftpWindow {
    pub(super) fn copy_desktop_locally(
        &mut self,
        prepared: Arc<Prepared>,
        visible: Arc<super::super::Directory>,
        rejected: usize,
    ) -> Task<Message> {
        let super::super::Location::Local(shown) = &visible.path else {
            return Task::none();
        };
        let shown = shown.clone();
        let ticket = Arc::new(());
        let operation = Arc::clone(&ticket);
        let failure = Arc::clone(&ticket);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&cancel);
        let (complete, done) = async_channel::bounded(1);
        let (send, receive) = async_channel::bounded(1);
        let thread = std::thread::Builder::new()
            .name("kortty-sftp-local-copy".into())
            .spawn(move || {
                let cancelled = || stopped.load(Ordering::Acquire);
                let report = copy(&prepared, rejected, &cancelled);
                let listing = files::list_local(&shown).ok().map(Arc::new);
                let status = report.label(listing.is_some());
                let _ = send.try_send(Reply {
                    ticket: operation,
                    status: status.into(),
                    listing,
                });
                drop(complete);
            });
        if let Err(error) = thread {
            self.status = Some(format!("Could not start the local copy ({error})."));
            return Task::none();
        }
        self.desktop_drop.local_job = Some((
            Job {
                ticket,
                cancel,
                done,
            },
            visible,
        ));
        Task::future(async move {
            let reply = receive.recv().await.unwrap_or_else(|_| Reply {
                ticket: failure,
                status: "The local copy worker stopped.".into(),
                listing: None,
            });
            Message::DesktopLocalCopyDone(reply)
        })
    }

    pub(in crate::sftp_window) fn finish_local_desktop_copy(
        &mut self,
        reply: Reply,
    ) -> Task<Message> {
        let Some((_, visible)) = self
            .desktop_drop
            .local_job
            .take_if(|(job, _)| Arc::ptr_eq(&job.ticket, &reply.ticket))
        else {
            return Task::none();
        };
        if self
            .local
            .directory
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &visible))
            && let Some(listing) = reply.listing.filter(|listing| listing.path == visible.path)
        {
            self.local.load(listing);
        }
        self.status = Some(reply.status.to_string());
        self.refresh_local_account_names()
    }
}

#[derive(Default)]
struct Report {
    copied: usize,
    skipped: usize,
    failed: usize,
    cancelled: bool,
    first_failure: Option<String>,
}

impl Report {
    fn label(&self, refreshed: bool) -> String {
        let mut text = if self.cancelled {
            "Local copy cancelled".to_owned()
        } else {
            "Local copy finished".to_owned()
        };
        text.push_str(&format!(
            " · {} copied · {} skipped · {} failed",
            self.copied, self.skipped, self.failed
        ));
        if let Some(error) = &self.first_failure {
            text.push_str(&format!(" · {error}"));
        }
        if !refreshed {
            text.push_str(" · Could not refresh the Local pane");
        }
        text
    }
}

fn copy(prepared: &Prepared, rejected: usize, cancelled: &dyn Fn() -> bool) -> Report {
    let stopped = std::cell::Cell::new(false);
    let cancelled = || {
        let cancelled = stopped.get() || cancelled();
        stopped.set(cancelled);
        cancelled
    };
    let mut report = Report {
        failed: rejected,
        ..Report::default()
    };
    let mut budget = Budget::default();
    for path in &prepared.sources {
        if cancelled() {
            break;
        }
        match copy_entry(path, prepared, &mut budget, &cancelled) {
            Ok(true) => report.copied += 1,
            Ok(false) => report.skipped += 1,
            Err(error) if error.kind() == io::ErrorKind::Interrupted && cancelled() => break,
            Err(error) => {
                report.failed += 1;
                report
                    .first_failure
                    .get_or_insert_with(|| files::display(&error.to_string()));
            }
        }
    }
    report.cancelled = stopped.get();
    report.skipped += budget.skipped;
    report
}

#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
    skipped: usize,
}

impl Budget {
    fn add_entries(&mut self, count: usize) -> io::Result<()> {
        self.entries = self
            .entries
            .checked_add(count)
            .filter(|count| *count <= MAX_ENTRIES)
            .ok_or_else(|| io::Error::other("The local copy exceeds 20,000 entries"))?;
        Ok(())
    }

    fn add_bytes(&mut self, count: u64) -> io::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(count)
            .filter(|bytes| *bytes <= MAX_BYTES)
            .ok_or_else(|| io::Error::other("The local copy exceeds 1 TiB"))?;
        Ok(())
    }
}

fn copy_entry(
    path: &Path,
    prepared: &Prepared,
    budget: &mut Budget,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<bool> {
    require_running(cancelled)?;
    budget.add_entries(1)?;
    let (source_parent, name) = path
        .parent()
        .zip(path.file_name())
        .ok_or_else(|| io::Error::other("Expected an absolute source filename"))?;
    let parent = Directory::open_path(source_parent)?;
    let target = Directory::open_path(&prepared.target)?;
    prepared.parent.require_directory(&target)?;
    if parent.same_as(&target)? {
        return Ok(false);
    }
    let entry = local_tree::entry(&parent, name)?;
    if matches!(entry, Entry::Skip) {
        return Ok(false);
    }
    let destination = vacant(
        &prepared.target,
        name,
        matches!(entry, Entry::Directory(_)),
        cancelled,
    )?;
    match entry {
        Entry::File(source) => {
            let stage = OrdinaryDownload::create_reviewed(&destination, &prepared.parent)?;
            let stamp = copy_contents(&source, stage.writer()?, budget, || {
                require_running(cancelled)
            })?;
            let verify = || {
                require_running(cancelled)?;
                if !parent.same_as(&Directory::open_path(source_parent)?)? {
                    return Err(source_changed());
                }
                require_same_file(&parent, name, &source, &stamp)
            };
            stage.publish_checked(verify)?;
        }
        Entry::Directory(source) => {
            let source_identity = Identity::directory(&source)?;
            for ancestor in prepared.target.ancestors() {
                if Identity::directory(&Directory::open_path(ancestor)?)? == source_identity {
                    return Err(io::Error::other("A folder cannot be copied into itself"));
                }
            }
            let mut cleanup_failed = false;
            let mut stage =
                Stage::create_reviewed(&destination, &prepared.parent, &mut cleanup_failed)?;
            let result = (|| {
                let witness = copy_directory(&source, stage.root(), &stage, 0, budget, cancelled)?;
                require_same_folder(&parent, name, &source)?;
                if !parent.same_as(&Directory::open_path(source_parent)?)? {
                    return Err(source_changed());
                }
                witness.verify(&source, cancelled)?;
                stage.publish(cancelled).map_err(|error| error.error)
            })();
            let cleanup = stage.cleanup();
            match (result, cleanup) {
                (Ok(()), Ok(())) => {}
                (Err(error), Ok(())) => return Err(error),
                (Ok(()), Err(cleanup)) => {
                    return Err(io::Error::other(format!(
                        "Folder copied; private staging cleanup failed ({cleanup})"
                    )));
                }
                (Err(error), Err(cleanup)) => {
                    return Err(io::Error::other(format!(
                        "{error}; private staging cleanup failed ({cleanup})"
                    )));
                }
            }
        }
        Entry::Skip => unreachable!(),
    }
    Ok(true)
}

struct FolderWitness {
    identity: Identity,
    stamp: LocalStamp,
    entries: Vec<(OsString, EntryWitness)>,
}

enum EntryWitness {
    Folder(FolderWitness),
    File(Identity, LocalStamp),
    Skipped,
}

impl FolderWitness {
    fn verify(&self, source: &Directory, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        require_running(cancelled)?;
        self.identity.require_directory(source)?;
        if LocalStamp::capture(&source.metadata()?) != self.stamp
            || source.names(self.entries.len())?.len() != self.entries.len()
        {
            return Err(source_changed());
        }
        for (name, witness) in &self.entries {
            require_running(cancelled)?;
            match (witness, local_tree::entry(source, name)?) {
                (EntryWitness::Folder(witness), Entry::Directory(folder)) => {
                    witness.verify(&folder, cancelled)?;
                }
                (EntryWitness::File(identity, stamp), Entry::File(file)) => {
                    identity.require(&file)?;
                    if LocalStamp::capture(&file.metadata()?) != *stamp {
                        return Err(source_changed());
                    }
                }
                (EntryWitness::Skipped, Entry::Skip) => {}
                _ => return Err(source_changed()),
            }
        }
        Ok(())
    }
}

fn copy_directory(
    source: &Directory,
    output: &Directory,
    stage: &Stage,
    depth: usize,
    budget: &mut Budget,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<FolderWitness> {
    require_running(cancelled)?;
    let stamp = LocalStamp::capture(&source.metadata()?);
    let names = source.names(MAX_ENTRIES - budget.entries)?;
    budget.add_entries(names.len())?;
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        require_running(cancelled)?;
        stage.verify_privacy()?;
        let witness = match local_tree::entry(source, &name)? {
            Entry::Directory(folder) => {
                if depth == MAX_DEPTH {
                    return Err(io::Error::other("The local copy exceeds 32 nested levels"));
                }
                let target = output.create_transfer_child(&name)?;
                let witness =
                    copy_directory(&folder, &target, stage, depth + 1, budget, cancelled)?;
                require_same_folder(source, &name, &folder)?;
                EntryWitness::Folder(witness)
            }
            Entry::File(file) => {
                let output = output.create_ordinary_file(&name)?;
                let verify = || {
                    require_running(cancelled)?;
                    stage.verify_privacy()
                };
                let stamp = copy_contents(&file, &output, budget, verify)?;
                require_same_file(source, &name, &file, &stamp)?;
                EntryWitness::File(Identity::capture(&file)?, stamp)
            }
            Entry::Skip => {
                budget.skipped += 1;
                EntryWitness::Skipped
            }
        };
        entries.push((name, witness));
    }
    if LocalStamp::capture(&source.metadata()?) != stamp {
        return Err(source_changed());
    }
    Ok(FolderWitness {
        identity: Identity::directory(source)?,
        stamp,
        entries,
    })
}

fn copy_contents(
    source: &File,
    mut output: &File,
    budget: &mut Budget,
    verify: impl Fn() -> io::Result<()>,
) -> io::Result<LocalStamp> {
    let stamp = LocalStamp::capture(&source.metadata()?);
    budget.add_bytes(stamp.size)?;
    let mut remaining = stamp.size;
    let mut source = source;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        verify()?;
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        remaining = remaining
            .checked_sub(read as u64)
            .ok_or_else(source_changed)?;
        output.write_all(&buffer[..read])?;
    }
    if remaining != 0 || LocalStamp::capture(&source.metadata()?) != stamp {
        return Err(source_changed());
    }
    output.sync_all()?;
    Ok(stamp)
}

fn vacant(
    directory: &Path,
    name: &OsStr,
    folder: bool,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<PathBuf> {
    for suffix in 1..=1000 {
        require_running(cancelled)?;
        let candidate = if suffix == 1 {
            directory.join(name)
        } else {
            let path = Path::new(name);
            let mut numbered = OsString::from(if folder {
                name
            } else {
                path.file_stem().unwrap_or(name)
            });
            numbered.push(format!(" ({suffix})"));
            if let Some(extension) = path.extension().filter(|_| !folder) {
                numbered.push(".");
                numbered.push(extension);
            }
            directory.join(numbered)
        };
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    }
    Err(io::Error::other("Could not find an unused local copy name"))
}

fn require_same_folder(parent: &Directory, name: &OsStr, expected: &Directory) -> io::Result<()> {
    if let Entry::Directory(current) = local_tree::entry(parent, name)?
        && current.same_as(expected)?
    {
        return Ok(());
    }
    Err(source_changed())
}

fn require_same_file(
    parent: &Directory,
    name: &OsStr,
    expected: &File,
    stamp: &LocalStamp,
) -> io::Result<()> {
    if let Entry::File(current) = local_tree::entry(parent, name)?
        && LocalStamp::capture(&current.metadata()?) == *stamp
        && LocalStamp::capture(&expected.metadata()?) == *stamp
    {
        return Identity::capture(expected)?.require(&current);
    }
    Err(source_changed())
}

fn source_changed() -> io::Error {
    io::Error::other("The local copy source changed")
}

fn require_running(cancelled: &dyn Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}
