//! Read-only review and one-use removal of selected file trees.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sftp_window::tree::snapshot::{
    self, DirectoryStamp, Entry as SnapshotEntry, Manifest, Relative,
};

use super::sync::removal::{Outcome, remove_reviewed};
use super::*;

pub struct Plan {
    manifest: Manifest,
    selected: HashSet<Vec<u8>>,
    paths: Vec<String>,
    worker: Weak<Control>,
    used: AtomicBool,
}

pub struct Request {
    pub(super) root: Location,
    pub(super) entries: Vec<Entry>,
    pub(super) worker: Weak<Control>,
}

impl Request {
    pub(in crate::sftp_window) fn directory(&self) -> &Location {
        &self.root
    }
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecursiveDeletePlan")
            .field("entries", &self.paths.len())
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn directory(&self) -> &Location {
        &self.manifest.root
    }

    pub(in crate::sftp_window) fn paths(&self) -> &[String] {
        &self.paths
    }
}

pub(super) fn review(
    client: &mut Client,
    root: Location,
    entries: &[Entry],
    worker: Weak<Control>,
    stopped: &impl Fn() -> bool,
) -> Result<Plan> {
    if matches!(root, Location::Remote(_)) && !client.supports_atomic_replace() {
        return Err(io::Error::other(
            "Recursive server deletion requires the SFTP POSIX rename extension",
        )
        .into());
    }
    let mut selected = HashSet::new();
    for entry in entries {
        if stopped() {
            return Err(io::Error::other("Deletion review stopped").into());
        }
        let name = match &entry.name {
            EntryName::Local(name) => name.to_str().ok_or_else(unsupported)?.as_bytes(),
            EntryName::Remote(name) => name.as_slice(),
        };
        let path = root.with(&entry.name).ok_or_else(unsupported)?;
        files::validate_remote_name(name)?;
        if !entry.recursive_delete_supported() || !selected.insert(name.to_vec()) {
            return Err(unsupported().into());
        }
        verify_change_source(client, &path, entry)?;
    }
    let manifest = snapshot::read_selected(client, root, &selected, stopped)?;
    for name in &selected {
        if !manifest.entries.keys().any(|path| path.bytes() == name) {
            return Err(io::Error::other("A selected entry disappeared during review").into());
        }
    }
    if manifest.entries.is_empty() || manifest.entries.len() > 4096 {
        return Err(
            io::Error::other("Recursive deletion review requires 1 to 4096 entries").into(),
        );
    }
    let mut paths = Vec::with_capacity(manifest.entries.len());
    for (path, entry) in &manifest.entries {
        if !entry.reviewable_for_deletion() {
            return Err(unsupported().into());
        }
        manifest.location(path)?;
        paths.push(path.review_label());
    }
    Ok(Plan {
        manifest,
        selected,
        paths,
        worker,
        used: AtomicBool::new(false),
    })
}

pub(super) fn apply(
    client: &mut Client,
    plan: &Plan,
    control: &Control,
    stopped: &impl Fn() -> bool,
) -> Result<Reply> {
    if !std::ptr::eq(plan.worker.as_ptr(), control) || plan.used.swap(true, Ordering::AcqRel) {
        return Err(io::Error::other(
            "This deletion review no longer belongs to the current connection",
        )
        .into());
    }
    let current =
        snapshot::read_selected(client, plan.directory().clone(), &plan.selected, stopped)?;
    if !plan.manifest.matches(&current) {
        return Err(
            io::Error::other("The selected tree changed. Review it again before deleting").into(),
        );
    }
    let mut changed_parents = BTreeMap::<Relative, DirectoryStamp>::new();
    let allowed = || stopped() || !control.route_allowed(&ClipboardPolicy::load());
    let mut completed = 0;
    let result = (|| {
        for (path, expected) in current.entries.iter().rev() {
            let target = current.location(path)?;
            let mut parent = current.parent(path).ok_or_else(unsupported)?;
            let parent_key = path.parent();
            if let Some(updated) = parent_key.as_ref().and_then(|key| changed_parents.get(key)) {
                parent.1 = updated.clone();
            }
            let updated;
            let expected =
                if let Some(DirectoryStamp::Remote(attributes)) = changed_parents.get(path) {
                    updated = SnapshotEntry::Remote(*attributes);
                    &updated
                } else {
                    expected
                };
            match remove_reviewed(client, &parent, &target, expected, &allowed)
                .map_err(|error| error.context(format!(" · {}", path.review_label())))?
            {
                Outcome::KeptNotEmpty => {
                    return Err(io::Error::other("A reviewed folder is no longer empty").into());
                }
                Outcome::Removed(warning) => {
                    completed += 1;
                    if let Some(error) = warning {
                        return Err(error);
                    }
                }
            }
            if let (Location::Remote(_), Some(key)) = (&target, parent_key) {
                let stamp = parent.1.verify(client, &parent.0, false)?;
                changed_parents.insert(key, stamp);
            }
        }
        Ok::<_, Failure>(())
    })();
    let mut note = format!(
        "Deleted {completed} of {} reviewed entries",
        plan.paths.len()
    );
    if let Err(error) = result {
        note.push_str(&format!(" · {}", error.message));
        if error.disconnected {
            return Err(Failure {
                message: note,
                ..error
            });
        }
    }
    changed(client, plan.directory(), note)
}

fn unsupported() -> io::Error {
    io::Error::other(
        "The selection contains an entry that cannot be reviewed for recursive deletion.",
    )
}
