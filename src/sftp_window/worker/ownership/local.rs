//! Review and apply Unix ownership and mode changes through anchored file descriptors.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata, Permissions};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::fs::{AtFlags, Gid, Stat, Uid, fchown, fstat, statat};

use crate::sftp_window::files::LocalStamp;
use crate::sftp_window::{EntryKind, local_tree};

use super::{
    AttemptFailure, Counts, Entry, EntryName, Failure, Location, MAX_TARGETS, Owners,
    PERMISSION_BITS, Reply, Result, ReviewRow, files, io, principals, retained_mode,
};

const MAX_ENTRIES: usize = 256;
const MAX_DEPTH: usize = 32;
const MAX_PATH_LABEL: usize = 4096;
const MAX_REVIEW_LABELS: usize = 256 * 1024;

pub struct Plan {
    directory: PathBuf,
    directory_label: String,
    roots: Vec<OsString>,
    recursive: bool,
    owners: Owners,
    principals: principals::Selection,
    tree: Tree,
    rows: Vec<ReviewRow>,
    used: AtomicBool,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalOwnershipPlan")
            .field("entries", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn directory_label(&self) -> &str {
        &self.directory_label
    }

    pub(in crate::sftp_window) fn rows(&self) -> &[ReviewRow] {
        &self.rows
    }

    pub(in crate::sftp_window) fn uids(&self) -> (Option<u32>, Option<u32>) {
        (self.owners.uid, self.owners.gid)
    }

    pub(in crate::sftp_window) fn principals(&self) -> &principals::Selection {
        &self.principals
    }

    pub(in crate::sftp_window) fn skipped(&self) -> usize {
        self.tree.skipped.len()
    }
}

#[derive(PartialEq, Eq)]
struct Tree {
    directory: (u64, u64),
    targets: Vec<Target>,
    folders: BTreeMap<PathBuf, (u64, u64)>,
    skipped: Vec<PathBuf>,
}

#[derive(PartialEq, Eq)]
struct Target {
    path: PathBuf,
    stamp: LocalStamp,
    uid: u32,
    gid: u32,
    mode: u32,
}

impl Target {
    fn row(&self) -> ReviewRow {
        ReviewRow {
            path: self.path.as_os_str().as_bytes().escape_ascii().to_string(),
            uid: self.uid,
            gid: self.gid,
            mode: self.mode,
            directory: self.stamp.kind == EntryKind::Directory,
        }
    }
}

pub(in crate::sftp_window) fn prepare(
    directory: PathBuf,
    selected: &[Entry],
    principals: principals::Selection,
    recursive: bool,
    guard: impl Fn() -> Result<()>,
) -> Result<Plan> {
    guard()?;
    if !(1..=MAX_TARGETS).contains(&selected.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Select between 1 and 64 local files or folders.",
        )
        .into());
    }
    let mut names = BTreeSet::new();
    let mut roots = Vec::with_capacity(selected.len());
    for entry in selected {
        let EntryName::Local(name) = &entry.name else {
            return Err(stale());
        };
        if !entry.kind.transferable() || !names.insert(name) {
            return Err(stale());
        }
        roots.push(files::local_name(name.as_bytes())?);
    }
    let directory_label = label(&directory)?;
    let tree = scan(&directory, &roots, Some(selected), recursive, &guard)?;
    let owners = principals.resolve(|kind, name| principals::local::resolve(kind, name, &guard))?;
    let rows = tree.targets.iter().map(Target::row).collect();
    Ok(Plan {
        directory,
        directory_label,
        roots,
        recursive,
        owners,
        principals,
        tree,
        rows,
        used: AtomicBool::new(false),
    })
}

fn scan(
    directory: &Path,
    roots: &[OsString],
    selected: Option<&[Entry]>,
    recursive: bool,
    guard: &impl Fn() -> Result<()>,
) -> Result<Tree> {
    guard()?;
    let parent = local_tree::Directory::open_path(directory)?;
    let mut walk = Walk {
        tree: Tree {
            directory: parent.identity()?,
            targets: Vec::new(),
            folders: BTreeMap::new(),
            skipped: Vec::new(),
        },
        remaining: MAX_ENTRIES,
        label_bytes: MAX_REVIEW_LABELS,
        recursive,
        guard,
    };
    for (index, name) in roots.iter().enumerate() {
        let reviewed = selected.map(|entries| &entries[index]);
        walk.entry(&parent, name, Path::new(name), 0, reviewed)?;
    }
    guard()?;
    if !parent.same_as(&local_tree::Directory::open_path(directory)?)? {
        return Err(stale());
    }
    Ok(walk.tree)
}

struct Walk<'a, F> {
    tree: Tree,
    remaining: usize,
    label_bytes: usize,
    recursive: bool,
    guard: &'a F,
}

impl<F: Fn() -> Result<()>> Walk<'_, F> {
    fn entry(
        &mut self,
        parent: &local_tree::Directory,
        name: &OsStr,
        path: &Path,
        depth: usize,
        reviewed: Option<&Entry>,
    ) -> Result<()> {
        (self.guard)()?;
        if depth > MAX_DEPTH {
            return Err(
                io::Error::other("The local ownership review exceeds 32 nested folders.").into(),
            );
        }
        self.remaining = self.remaining.checked_sub(1).ok_or_else(limit)?;
        self.label_bytes = self
            .label_bytes
            .checked_sub(label(path)?.len())
            .ok_or_else(limit)?;
        let entry = local_tree::entry(parent, name)?;
        let file = match &entry {
            local_tree::Entry::Directory(directory) => directory.file(),
            local_tree::Entry::File(file) => file,
            local_tree::Entry::Skip => return self.skip(path, depth),
        };
        let metadata = file.metadata()?;
        if metadata.is_file() && metadata.nlink() != 1 {
            return self.skip(path, depth);
        }
        if let Some(reviewed) = reviewed
            && (reviewed.kind != files::local_kind(&metadata)
                || reviewed.owner != Some(metadata.uid())
                || reviewed.group != Some(metadata.gid())
                || reviewed.mode != Some(metadata.mode() & 0o777)
                || (metadata.is_file() && reviewed.size != Some(metadata.len())))
        {
            return Err(stale());
        }
        let stamp = LocalStamp::capture(&metadata);
        if let local_tree::Entry::Directory(directory) = &entry {
            self.tree
                .folders
                .insert(path.to_owned(), directory.identity()?);
            if self.recursive {
                let mut names = directory.names(self.remaining)?;
                names.sort_unstable();
                for name in names {
                    self.entry(directory, &name, &path.join(&name), depth + 1, None)?;
                }
                if LocalStamp::capture(&file.metadata()?) != stamp {
                    return Err(stale());
                }
            }
        }
        // Change descendants before setting permissions that may close their parent.
        self.tree.targets.push(Target {
            path: path.to_owned(),
            stamp,
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & PERMISSION_BITS,
        });
        Ok(())
    }

    fn skip(&mut self, path: &Path, depth: usize) -> Result<()> {
        if depth == 0 {
            return Err(io::Error::other(
                "Selected local entries must be regular single-link files or directories.",
            )
            .into());
        }
        self.tree.skipped.push(path.to_owned());
        Ok(())
    }
}

pub(in crate::sftp_window) fn apply(plan: &Plan, guard: impl Fn() -> Result<()>) -> Result<Reply> {
    let current = scan(&plan.directory, &plan.roots, None, plan.recursive, &guard)?;
    if current != plan.tree {
        return Err(stale());
    }
    plan.principals.verify(plan.owners, |kind, name| {
        principals::local::resolve(kind, name, &guard)
    })?;
    guard()?;
    if plan.used.swap(true, Ordering::AcqRel) {
        return Err(io::Error::other("This local ownership review was already used.").into());
    }
    let mut counts = Counts::default();
    let total = plan.tree.targets.len();
    let includes_mode = plan.principals.mode().is_some();
    for target in &plan.tree.targets {
        match apply_target(plan, target, &guard) {
            Ok(true) => counts.changed += 1,
            Ok(false) => counts.matched += 1,
            Err(failure) => {
                counts.uncertain = failure.uncertain;
                counts.ownership_only = failure.ownership_verified;
                if !counts.has_progress() {
                    return Err(failure.error);
                }
                let note = counts.note(total, includes_mode);
                if failure.error.disconnected || guard().is_err() {
                    return Err(failure.error.context(format!(" · {note}")));
                }
                return finish(plan, format!("{note} · {}", failure.error.message), &guard);
            }
        }
    }
    finish(plan, counts.note(total, includes_mode), &guard)
}

fn apply_target(
    plan: &Plan,
    target: &Target,
    guard: &impl Fn() -> Result<()>,
) -> std::result::Result<bool, AttemptFailure> {
    guard()?;
    let (parent, name) = target_parent(plan, &target.path)?;
    let entry = local_tree::entry(&parent, name).map_err(Failure::from)?;
    let file = opened(&entry)?;
    let before = file.metadata().map_err(Failure::from)?;
    if !single_link(&before) || LocalStamp::capture(&before) != target.stamp {
        return Err(stale().into());
    }
    let uid = plan.owners.uid.filter(|&id| id != before.uid());
    let gid = plan.owners.gid.filter(|&id| id != before.gid());
    guard()?;
    let ownership_changed = uid.is_some() || gid.is_some();
    let mut current_mode = before.mode() & PERMISSION_BITS;
    if ownership_changed {
        fchown(file, uid.map(Uid::from_raw), gid.map(Gid::from_raw))
            .map_err(io::Error::from)
            .map_err(Failure::from)?;
        current_mode = guard()
            .and_then(|()| verify_change(plan, target, file, &before, None))
            .map_err(|error| AttemptFailure {
                error,
                uncertain: true,
                ownership_verified: false,
            })?;
    }
    let Some(mode) = plan.principals.mode().filter(|&mode| mode != current_mode) else {
        return Ok(ownership_changed);
    };
    let unmodified_mode = |error| AttemptFailure {
        error,
        uncertain: false,
        ownership_verified: ownership_changed,
    };
    guard().map_err(unmodified_mode)?;
    file.set_permissions(Permissions::from_mode(mode))
        .map_err(Failure::from)
        .map_err(unmodified_mode)?;
    guard()
        .and_then(|()| verify_change(plan, target, file, &before, Some(mode)))
        .map_err(|error| AttemptFailure {
            error,
            uncertain: true,
            ownership_verified: ownership_changed,
        })?;
    Ok(true)
}

fn verify_change(
    plan: &Plan,
    target: &Target,
    file: &File,
    before: &Metadata,
    requested_mode: Option<u32>,
) -> Result<u32> {
    // The held descriptor stays readable to fstat even after setting mode 000.
    // A separate no-follow stat through the checked parent verifies the name.
    let after = fstat(file).map_err(io::Error::from)?;
    let (parent, name) = target_parent(plan, &target.path)?;
    let current =
        statat(parent.file(), name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    let mode = u32::try_from(u64::from(after.st_mode)).map_err(io::Error::other)? & PERMISSION_BITS;
    let correct_mode = requested_mode.map_or_else(
        || retained_mode(Some(before.mode() & PERMISSION_BITS), Some(mode)),
        |requested| mode == requested,
    );
    if !correct_mode
        || after.st_uid != plan.owners.uid.unwrap_or(before.uid())
        || after.st_gid != plan.owners.gid.unwrap_or(before.gid())
        || u64::try_from(after.st_size).ok() != Some(before.len())
        || after.st_mtime != before.mtime()
        || after.st_mtime_nsec != before.mtime_nsec()
        || (before.is_file() && after.st_nlink != 1)
        || !same_entry(&after, &current)
    {
        return Err(io::Error::other(
            "Could not verify the requested local ownership, permissions or entry metadata.",
        )
        .into());
    }
    Ok(mode)
}

fn same_entry(opened: &Stat, named: &Stat) -> bool {
    opened.st_dev == named.st_dev
        && opened.st_ino == named.st_ino
        && opened.st_mode == named.st_mode
        && opened.st_uid == named.st_uid
        && opened.st_gid == named.st_gid
        && opened.st_nlink == named.st_nlink
        && opened.st_size == named.st_size
        && opened.st_mtime == named.st_mtime
        && opened.st_mtime_nsec == named.st_mtime_nsec
        && opened.st_ctime == named.st_ctime
        && opened.st_ctime_nsec == named.st_ctime_nsec
}

fn target_parent<'a>(plan: &Plan, path: &'a Path) -> Result<(local_tree::Directory, &'a OsStr)> {
    let mut current = local_tree::Directory::open_path(&plan.directory)?;
    if current.identity()? != plan.tree.directory {
        return Err(stale());
    }
    let mut parent = PathBuf::new();
    let mut components = path.components().peekable();
    while let Some(Component::Normal(name)) = components.next() {
        if components.peek().is_none() {
            return Ok((current, name));
        }
        let local_tree::Entry::Directory(directory) = local_tree::entry(&current, name)? else {
            return Err(stale());
        };
        parent.push(name);
        if plan.tree.folders.get(&parent) != Some(&directory.identity()?) {
            return Err(stale());
        }
        current = directory;
    }
    Err(stale())
}

fn opened(entry: &local_tree::Entry) -> Result<&File> {
    match entry {
        local_tree::Entry::Directory(directory) => Ok(directory.file()),
        local_tree::Entry::File(file) => Ok(file),
        local_tree::Entry::Skip => Err(stale()),
    }
}

fn single_link(metadata: &Metadata) -> bool {
    metadata.is_dir() || (metadata.is_file() && metadata.nlink() == 1)
}

fn finish(plan: &Plan, note: String, guard: &impl Fn() -> Result<()>) -> Result<Reply> {
    if let Err(error) = guard() {
        return Err(error.context(format!(" · {note}")));
    }
    let refreshed = (|| -> io::Result<_> {
        let directory = local_tree::Directory::open_path(&plan.directory)?;
        if directory.identity()? != plan.tree.directory {
            return Err(io::Error::other("The local review folder was replaced."));
        }
        let listing = files::list_local(&plan.directory)?;
        if listing.path != Location::Local(plan.directory.clone())
            || !directory.same_as(&local_tree::Directory::open_path(&plan.directory)?)?
        {
            return Err(io::Error::other("The local review folder was replaced."));
        }
        Ok(listing)
    })();
    match refreshed {
        Ok(directory) => Ok(Reply::Changed {
            directory: Some(Arc::new(directory)),
            note,
        }),
        Err(error) => Ok(Reply::Changed {
            directory: None,
            note: format!("{note} · refresh failed ({error})"),
        }),
    }
}

fn label(path: &Path) -> Result<String> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.escape_ascii().count() > MAX_PATH_LABEL {
        return Err(
            io::Error::other("An exact local path exceeds the ownership review limit.").into(),
        );
    }
    Ok(bytes.escape_ascii().to_string())
}

fn stale() -> Failure {
    io::Error::other("The selected local entries changed or could not be verified. Refresh the folder and review again.").into()
}

fn limit() -> Failure {
    io::Error::other("Local ownership review exceeds 256 entries or its path budget.").into()
}

#[cfg(test)]
mod tests;
