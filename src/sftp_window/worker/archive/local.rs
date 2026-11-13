//! Review and publish local archives using the file browser's anchored tree APIs.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use crate::archive_codec::{
    MAX_ARCHIVE_BYTES, MAX_ENTRIES, MAX_INPUT_BYTES, MAX_PATH_BYTES, Member, encode,
};
use crate::private_files::FileStamp;
use crate::scheduler_window::{ArchiveExclusions, ArchiveFormat, MAX_ARCHIVE_SOURCES};
use crate::sftp_window::files::{self, LocalStamp};
use crate::sftp_window::local_tree::{self, Directory as LocalDirectory, Entry as LocalEntry};
use crate::sftp_window::worker::resume::persist::Identity;
use crate::sftp_window::{Directory, Entry, EntryName, Location, StagedDownload};

use super::super::{
    Reply, Result,
    sync::{Cancel, review_root},
};
use super::Options;

const MAX_DEPTH: usize = 32;
const MAX_COMPONENTS: usize = 64;

pub(crate) struct Request {
    directory: PathBuf,
    selected: Vec<Entry>,
    destination: PathBuf,
    format: ArchiveFormat,
    level: u8,
    exclusions: ArchiveExclusions,
}

impl Request {
    pub(in crate::sftp_window) fn new(
        directory: PathBuf,
        selected: Vec<Entry>,
        options: Options,
    ) -> io::Result<Self> {
        validate_path(&directory)?;
        if !(1..=MAX_ARCHIVE_SOURCES).contains(&selected.len()) {
            return Err(invalid("Select between one and 16 local files or folders"));
        }
        let mut names = BTreeSet::new();
        for entry in &selected {
            let name = local_name(entry)?;
            safe_component(name)?;
            if !entry.kind.transferable() || !names.insert(name) {
                return Err(invalid("Select distinct ordinary local files or folders"));
            }
        }
        if !options.format.valid_level(options.level) {
            return Err(invalid(options.format.level_hint()));
        }
        let destination = destination(&directory, &options.name, options.format)?;
        if names
            .iter()
            .any(|name| destination.starts_with(directory.join(name)))
        {
            return Err(overlap());
        }
        let exclusions = ArchiveExclusions::compile_local(&options.excludes).map_err(invalid)?;
        Ok(Self {
            directory,
            selected,
            destination,
            format: options.format,
            level: options.level,
            exclusions,
        })
    }
}

fn destination(directory: &Path, name: &str, format: ArchiveFormat) -> io::Result<PathBuf> {
    let path = Path::new(name);
    let path = if path.is_absolute() {
        let last = name.rsplit(['/', '\\']).next();
        if path.file_name().and_then(OsStr::to_str) != last {
            return Err(invalid("Enter a complete archive file path"));
        }
        path.to_path_buf()
    } else {
        safe_component(path.as_os_str())?;
        directory.join(path)
    };
    validate_path(&path)?;
    if !path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with(format.extension()))
    {
        return Err(invalid(
            "Choose a filename with the extension for the selected archive format",
        ));
    }
    Ok(path)
}

pub struct Plan {
    request: Request,
    manifest: Manifest,
    destination_parent: Identity,
    destination: String,
    rows: Vec<(String, bool)>,
    used: AtomicBool,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalArchivePlan")
            .field("entries", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn destination(&self) -> &str {
        &self.destination
    }

    pub(in crate::sftp_window) fn format(&self) -> ArchiveFormat {
        self.request.format
    }

    pub(in crate::sftp_window) fn entries(&self) -> &[(String, bool)] {
        &self.rows
    }

    pub(in crate::sftp_window) fn input_bytes(&self) -> u64 {
        self.manifest.input_bytes
    }

    pub(in crate::sftp_window) fn excluded_count(&self) -> usize {
        self.manifest.excluded
    }
}

pub(in crate::sftp_window) fn prepare(request: Request, cancel: &Cancel) -> Result<Plan> {
    let stopped = || cancel.stopped();
    let result = (|| {
        let destination = review_root(&Location::Local(request.destination.clone()))?;
        let manifest = scan(&request, &stopped)?;
        let destination_parent = verify_destination(&request, &manifest, &stopped)?;
        let rows = manifest
            .members
            .iter()
            .map(|member| (member.name.clone(), member.directory))
            .collect();
        checkpoint(&stopped)?;
        Ok(Plan {
            request,
            manifest,
            destination_parent,
            destination,
            rows,
            used: AtomicBool::new(false),
        })
    })();
    cancel.finish();
    result
}

pub(in crate::sftp_window) fn apply(
    plan: &Plan,
    password: Option<&str>,
    cancel: &Cancel,
) -> Result<Reply> {
    let stopped = || cancel.stopped();
    let result = apply_checked(plan, password, &stopped);
    cancel.finish();
    result.map_err(Into::into)
}

fn apply_checked(
    plan: &Plan,
    password: Option<&str>,
    stopped: &impl Fn() -> bool,
) -> io::Result<Reply> {
    checkpoint(stopped)?;
    if plan.used.swap(true, Ordering::AcqRel) {
        return Err(invalid("This local archive review was already used"));
    }
    let request = &plan.request;
    verify_review(plan, stopped)?;
    let bytes = encode(
        &plan.manifest.members,
        request.format,
        request.level,
        password,
        stopped,
        |index, output| copy_member(plan, index, output, stopped),
    )?;
    checkpoint(stopped)?;
    let stage = StagedDownload::create_reviewed(&request.destination, &plan.destination_parent)?;
    let mut writer = stage.writer()?;
    for block in bytes.chunks(64 * 1024) {
        checkpoint(stopped)?;
        writer.write_all(block)?;
    }
    writer.sync_all()?;
    let stamp = FileStamp::read(writer)?;
    verify_review(plan, stopped)?;
    stage.verify_partial(&stamp)?;
    stage.publish(stopped)?;
    let mut note = format!(
        "Created {} archive with {} entries and {} bytes · {}",
        plan.format(),
        plan.entries().len(),
        bytes.len(),
        plan.destination(),
    );
    let directory = if stopped() {
        None
    } else {
        refresh(plan, &mut note)
    };
    Ok(Reply::Changed { directory, note })
}

fn verify_review(plan: &Plan, stopped: &impl Fn() -> bool) -> io::Result<()> {
    if scan(&plan.request, stopped)? != plan.manifest
        || verify_destination(&plan.request, &plan.manifest, stopped)? != plan.destination_parent
    {
        return Err(stale());
    }
    checkpoint(stopped)
}

fn verify_destination(
    request: &Request,
    manifest: &Manifest,
    stopped: &impl Fn() -> bool,
) -> io::Result<Identity> {
    let (path, name) = request
        .destination
        .parent()
        .zip(request.destination.file_name())
        .ok_or_else(|| invalid("Enter a complete archive file path"))?;
    let parent = LocalDirectory::open_path(path)?;
    let identity = Identity::directory(&parent)?;
    // Check physical ancestors too. Alternate Windows spelling and other aliases
    // can reach the selected directory without sharing its text path.
    for ancestor in path.ancestors() {
        checkpoint(stopped)?;
        let current = Identity::directory(&LocalDirectory::open_path(ancestor)?)?;
        if manifest.roots.contains(&current) {
            return Err(overlap());
        }
    }
    match local_tree::entry(&parent, name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "The local archive destination already exists",
            ));
        }
    }
    if !parent.same_as(&LocalDirectory::open_path(path)?)? {
        return Err(stale());
    }
    Ok(identity)
}

#[derive(Eq, PartialEq)]
struct Manifest {
    directory: Identity,
    roots: Vec<Identity>,
    nodes: BTreeMap<PathBuf, NodeStamp>,
    members: Vec<Member>,
    member_paths: Vec<PathBuf>,
    input_bytes: u64,
    excluded: usize,
}

#[derive(Eq, PartialEq)]
struct NodeStamp {
    identity: Identity,
    stamp: LocalStamp,
}

impl NodeStamp {
    fn directory(directory: &LocalDirectory, metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            identity: Identity::directory(directory)?,
            stamp: LocalStamp::capture(metadata),
        })
    }

    fn file(file: &File, metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            identity: Identity::capture(file)?,
            stamp: LocalStamp::capture(metadata),
        })
    }

    fn require_file(&self, file: &File) -> io::Result<()> {
        if Self::file(file, &file.metadata()?)? != *self {
            return Err(stale());
        }
        Ok(())
    }
}

fn scan(request: &Request, stopped: &impl Fn() -> bool) -> io::Result<Manifest> {
    checkpoint(stopped)?;
    let directory = LocalDirectory::open_path(&request.directory)?;
    let mut walk = Walk {
        request,
        stopped,
        remaining: MAX_ENTRIES - request.selected.len(),
        path_bytes: 0,
        manifest: Manifest {
            directory: Identity::directory(&directory)?,
            roots: Vec::new(),
            nodes: BTreeMap::new(),
            members: Vec::new(),
            member_paths: Vec::new(),
            input_bytes: 0,
            excluded: 0,
        },
    };
    for selected in &request.selected {
        let name = local_name(selected)?;
        walk.inspect(&directory, Path::new(name), "", 0, Some(selected))?;
    }
    if !directory.same_as(&LocalDirectory::open_path(&request.directory)?)? {
        return Err(stale());
    }
    if walk.manifest.members.is_empty() {
        return Err(invalid(
            "There are no archive members after applying the exclusions and format",
        ));
    }
    if request.format.stores_uncompressed(request.level)
        && walk.manifest.input_bytes > MAX_ARCHIVE_BYTES as u64
    {
        return Err(invalid("The archive output exceeds 64 MiB"));
    }
    validate_members(&walk.manifest.members)?;
    checkpoint(stopped)?;
    Ok(walk.manifest)
}

struct Walk<'a, F> {
    request: &'a Request,
    stopped: &'a F,
    remaining: usize,
    path_bytes: usize,
    manifest: Manifest,
}

impl<F: Fn() -> bool> Walk<'_, F> {
    fn inspect(
        &mut self,
        parent: &LocalDirectory,
        path: &Path,
        member_parent: &str,
        depth: usize,
        selected: Option<&Entry>,
    ) -> io::Result<()> {
        checkpoint(self.stopped)?;
        if depth > MAX_DEPTH {
            return Err(tree_limit());
        }
        let name = path.file_name().ok_or_else(stale)?;
        let component = safe_component(name)?;
        let entry = local_tree::entry(parent, name)?;
        let (metadata, node) = match &entry {
            LocalEntry::Directory(directory) => {
                let metadata = directory_metadata(directory)?;
                let stamp = NodeStamp::directory(directory, &metadata)?;
                (metadata, stamp)
            }
            LocalEntry::File(file) => {
                let metadata = file.metadata()?;
                let stamp = NodeStamp::file(file, &metadata)?;
                (metadata, stamp)
            }
            LocalEntry::Skip => {
                return Err(invalid(
                    "Archive sources must not contain links, reparse points or special files",
                ));
            }
        };
        if selected.is_some_and(|selected| !matches_listing(selected, &metadata)) {
            return Err(stale());
        }
        let directory = metadata.is_dir();
        let omit_root = self.request.format.is_zip() && depth == 0 && directory;
        let member_name = if omit_root {
            String::new()
        } else if member_parent.is_empty() {
            component.to_owned()
        } else {
            format!("{member_parent}/{component}")
        };
        let absolute = absolute_glob_path(&self.request.directory.join(path))?;
        self.path_bytes += absolute.len() + member_name.len();
        if self.path_bytes > MAX_PATH_BYTES {
            return Err(tree_limit());
        }
        let excluded = self
            .request
            .exclusions
            .matches_paths(&absolute, &member_name);
        if depth == 0 && directory {
            self.manifest.roots.push(node.identity.clone());
        }
        if excluded {
            self.manifest.excluded += 1;
        } else {
            if !directory || !self.request.format.is_zip() {
                self.add_member(path, member_name.clone(), &node.stamp, directory)?;
            }
            if let LocalEntry::Directory(directory) = &entry {
                let mut names = directory.names(self.remaining)?;
                self.remaining -= names.len();
                names.sort_unstable();
                for name in names {
                    self.inspect(directory, &path.join(name), &member_name, depth + 1, None)?;
                }
                let LocalEntry::Directory(current) = local_tree::entry(parent, name)? else {
                    return Err(stale());
                };
                if NodeStamp::directory(&current, &directory_metadata(&current)?)? != node {
                    return Err(stale());
                }
            }
        }
        if self
            .manifest
            .nodes
            .insert(path.to_path_buf(), node)
            .is_some()
        {
            return Err(stale());
        }
        Ok(())
    }

    fn add_member(
        &mut self,
        path: &Path,
        name: String,
        stamp: &LocalStamp,
        directory: bool,
    ) -> io::Result<()> {
        let size = if directory { 0 } else { stamp.size };
        if size > MAX_INPUT_BYTES - self.manifest.input_bytes {
            return Err(invalid("The archive sources exceed 256 MiB"));
        }
        self.manifest.input_bytes += size;
        let modified = stamp
            .modified
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_secs());
        self.manifest.members.push(Member {
            name,
            directory,
            size,
            modified,
        });
        self.manifest.member_paths.push(path.to_path_buf());
        Ok(())
    }
}

fn validate_members(members: &[Member]) -> io::Result<()> {
    let collision = || invalid("Archive member names collide after applying the selected format");
    let mut names = BTreeMap::new();
    for member in members {
        let name = if cfg!(windows) {
            member.name.to_lowercase()
        } else {
            member.name.clone()
        };
        if names.insert(name, member.directory).is_some() {
            return Err(collision());
        }
    }
    for name in names.keys() {
        for (index, _) in name.match_indices('/') {
            if names.get(&name[..index]) == Some(&false) {
                return Err(collision());
            }
        }
    }
    Ok(())
}

fn copy_member(
    plan: &Plan,
    index: usize,
    output: &mut dyn Write,
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    checkpoint(stopped)?;
    let relative = &plan.manifest.member_paths[index];
    let path = plan.request.directory.join(relative);
    let (parent_path, name) = path.parent().zip(path.file_name()).ok_or_else(stale)?;
    let expected = plan.manifest.nodes.get(relative).ok_or_else(stale)?;
    let parent_identity = match relative
        .parent()
        .and_then(|path| plan.manifest.nodes.get(path))
    {
        Some(node) => &node.identity,
        None => &plan.manifest.directory,
    };
    let parent = LocalDirectory::open_path(parent_path)?;
    parent_identity.require_directory(&parent)?;
    let LocalEntry::File(file) = local_tree::entry(&parent, name)? else {
        return Err(stale());
    };
    expected.require_file(&file)?;
    let mut source = &file;
    if io::copy(
        &mut Read::by_ref(&mut source).take(expected.stamp.size),
        output,
    )? != expected.stamp.size
        || source.read(&mut [0])? != 0
    {
        return Err(stale());
    }
    checkpoint(stopped)?;
    expected.require_file(&file)?;
    let LocalEntry::File(current) = local_tree::entry(&parent, name)? else {
        return Err(stale());
    };
    expected.require_file(&current)?;
    if !parent.same_as(&LocalDirectory::open_path(parent_path)?)? {
        return Err(stale());
    }
    Ok(())
}

fn refresh(plan: &Plan, note: &mut String) -> Option<Arc<Directory>> {
    let path = &plan.request.directory;
    let listing = (|| -> io::Result<_> {
        let expected = &plan.manifest.directory;
        expected.require_directory(&LocalDirectory::open_path(path)?)?;
        let directory = files::list_local(path)?;
        expected.require_directory(&LocalDirectory::open_path(path)?)?;
        if directory.path != Location::Local(path.clone()) {
            return Err(stale());
        }
        Ok(directory)
    })();
    match listing {
        Ok(directory) => Some(Arc::new(directory)),
        Err(error) => {
            note.push_str(&format!(
                " · refresh failed ({})",
                files::display(&error.to_string())
            ));
            None
        }
    }
}

fn matches_listing(entry: &Entry, metadata: &Metadata) -> bool {
    if entry.kind != files::local_kind(metadata)
        || entry.size != metadata.is_file().then_some(metadata.len())
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        entry.owner == Some(metadata.uid())
            && entry.group == Some(metadata.gid())
            && entry.mode == Some(metadata.mode() & 0o777)
    }
    #[cfg(windows)]
    true
}

fn directory_metadata(directory: &LocalDirectory) -> io::Result<Metadata> {
    #[cfg(unix)]
    return directory.file().metadata();
    #[cfg(windows)]
    directory.metadata()
}

fn local_name(entry: &Entry) -> io::Result<&OsStr> {
    match &entry.name {
        EntryName::Local(name) => Ok(name),
        EntryName::Remote(_) => Err(stale()),
    }
}

fn safe_component(name: &OsStr) -> io::Result<&str> {
    let name = name
        .to_str()
        .ok_or_else(|| invalid("Archive paths must use valid UTF-8"))?;
    files::validate_transfer_name(name.as_bytes())?;
    if name.contains(['<', '>', '"', '|', '?', '*']) || name.chars().any(char::is_control) {
        return Err(invalid(
            "Archive paths must have ordinary file-name components",
        ));
    }
    Ok(name)
}

fn validate_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > 4096 {
        return Err(invalid(
            "Enter an absolute local archive path of at most 4 KiB",
        ));
    }
    if path.components().count() > MAX_COMPONENTS {
        return Err(tree_limit());
    }
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                safe_component(name)?;
            }
            #[cfg(windows)]
            Component::Prefix(prefix)
                if matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_)
                        | std::path::Prefix::VerbatimDisk(_)
                        | std::path::Prefix::UNC(_, _)
                        | std::path::Prefix::VerbatimUNC(_, _)
                ) => {}
            _ => {
                return Err(invalid(
                    "Enter an absolute local archive path with ordinary components",
                ));
            }
        }
    }
    Ok(())
}

fn absolute_glob_path(path: &Path) -> io::Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| invalid("Archive paths must use valid UTF-8"))?;
    #[cfg(unix)]
    return Ok(path.to_owned());
    #[cfg(windows)]
    {
        let path = path
            .strip_prefix(r"\\?\")
            .unwrap_or(path)
            .replace('\\', "/");
        Ok(if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        })
    }
}

fn checkpoint(stopped: &impl Fn() -> bool) -> io::Result<()> {
    if stopped() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Local archive cancelled",
        ));
    }
    Ok(())
}

fn tree_limit() -> io::Error {
    invalid("The local archive sources exceed the entry, depth or path budget")
}

fn overlap() -> io::Error {
    invalid("Choose an archive destination outside every selected source")
}

fn stale() -> io::Error {
    invalid("The selected local archive sources or destination changed. Refresh and review again.")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests;
