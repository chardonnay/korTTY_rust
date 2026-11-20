//! One-use cleanup of named regular transfer parts in the displayed folder.

use std::collections::HashSet;
use std::fmt;

use crate::sftp_window::partials;
use crate::sftp_window::tree::snapshot::{self, DirectoryStamp, Kind, Manifest};

use super::sync::removal::{Outcome, remove_reviewed, remove_reviewed_local};
use super::*;

pub(in crate::sftp_window) const MAX_PARTS: usize = 4096;

pub struct Request {
    pub(super) directory: Location,
    pub(super) entries: Vec<Entry>,
    pub(super) worker: Option<Weak<Control>>,
}

impl Request {
    pub(in crate::sftp_window) fn directory(&self) -> &Location {
        &self.directory
    }

    pub(in crate::sftp_window) fn local(directory: Location, entries: Vec<Entry>) -> Self {
        Self {
            directory,
            entries,
            worker: None,
        }
    }
}

pub struct Plan {
    manifest: Manifest,
    paths: Vec<String>,
    worker: Option<Weak<Control>>,
    used: AtomicBool,
}

impl fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PartialCleanupPlan")
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

fn names(entries: &[Entry]) -> Result<HashSet<Vec<u8>>> {
    if entries.len() > MAX_PARTS {
        return Err(io::Error::other("The folder contains more than 4,096 transfer parts").into());
    }
    let mut names = HashSet::with_capacity(entries.len());
    for entry in entries {
        let Some(name) = partials::candidate(entry) else {
            return Err(
                io::Error::other("Cleanup requires valid regular transfer part names").into(),
            );
        };
        if !partials::active(name) {
            names.insert(name.to_vec());
        }
    }
    if names.is_empty() {
        return Err(io::Error::other("No inactive transfer parts in this folder").into());
    }
    Ok(names)
}

fn reviewed(
    manifest: Manifest,
    names: &HashSet<Vec<u8>>,
    worker: Option<Weak<Control>>,
) -> Result<Plan> {
    let mut paths = Vec::with_capacity(names.len());
    for (path, entry) in &manifest.entries {
        if entry.kind() != Kind::File
            || !names.contains(path.bytes())
            || manifest.location(path).is_err()
        {
            return Err(io::Error::other(
                "A transfer part changed during review. Refresh the folder.",
            )
            .into());
        }
        paths.push(path.review_label());
    }
    if paths.len() != names.len() {
        return Err(io::Error::other(
            "A transfer part disappeared during review. Refresh the folder.",
        )
        .into());
    }
    Ok(Plan {
        manifest,
        paths,
        worker,
        used: AtomicBool::new(false),
    })
}

pub(super) fn review_local(request: Request, stopped: &impl Fn() -> bool) -> Result<Plan> {
    let Location::Local(root) = request.directory else {
        return Err(io::Error::other("Expected a local cleanup folder").into());
    };
    let names = names(&request.entries)?;
    let manifest = snapshot::read_local_selected(&root, &names, stopped)?;
    reviewed(manifest, &names, None)
}

pub(super) fn review_remote(
    client: &mut Client,
    request: Request,
    control: &Control,
    stopped: &impl Fn() -> bool,
) -> Result<Plan> {
    if !matches!(&request.directory, Location::Remote(_))
        || !request
            .worker
            .as_ref()
            .is_some_and(|worker| std::ptr::eq(worker.as_ptr(), control))
    {
        return Err(io::Error::other("Cleanup review belongs to another connection").into());
    }
    let names = names(&request.entries)?;
    let manifest = snapshot::read_selected(client, request.directory, &names, stopped)?;
    reviewed(manifest, &names, request.worker)
}

fn remove_parts(
    plan: &Plan,
    stopped: &impl Fn() -> bool,
    mut remove: impl FnMut(&(Location, DirectoryStamp), &Location, &snapshot::Entry) -> Result<Outcome>,
) -> Result<String> {
    if plan.used.swap(true, Ordering::AcqRel) {
        return Err(io::Error::other("This cleanup review has already been used").into());
    }
    let mut removed = 0;
    let mut failures = 0;
    let mut first = None;
    let mut cancelled = false;
    for (name, expected) in &plan.manifest.entries {
        if stopped() {
            cancelled = true;
            break;
        }
        let result = (|| {
            let _reservation = partials::reserve(name.bytes())
                .ok_or_else(|| io::Error::other("Transfer part is in use"))?;
            let parent = plan
                .manifest
                .parent(name)
                .ok_or_else(|| io::Error::other("Missing reviewed parent"))?;
            let path = plan.manifest.location(name)?;
            remove(&parent, &path, expected)
        })();
        let error = match result {
            Ok(Outcome::Removed(warning)) => {
                removed += 1;
                let Some(error) = warning else { continue };
                error
            }
            Ok(Outcome::KeptNotEmpty) => {
                io::Error::other("The part changed into a nonempty folder").into()
            }
            Err(error) => error,
        };
        if error.disconnected {
            return Err(error.context(format!(
                " · Removed {removed} of {} reviewed transfer parts",
                plan.paths.len()
            )));
        }
        failures += 1;
        first.get_or_insert_with(|| format!("{} ({})", name.review_label(), error.message));
    }
    let mut note = format!(
        "Removed {removed} of {} reviewed transfer parts",
        plan.paths.len()
    );
    if cancelled {
        note.push_str(" · Cleanup stopped");
    }
    if let Some(error) = first {
        note.push_str(&format!(" · {failures} failure(s) · {error}"));
    }
    Ok(note)
}

pub(super) fn apply_local(plan: &Plan, stopped: &impl Fn() -> bool) -> Result<Reply> {
    let Location::Local(root) = plan.directory() else {
        return Err(io::Error::other("Expected a reviewed local cleanup folder").into());
    };
    let note = remove_parts(plan, stopped, |parent, path, entry| {
        remove_reviewed_local(parent, path, entry, stopped)
    })?;
    Ok(match files::list_local(root) {
        Ok(directory) => Reply::Changed {
            directory: Some(Arc::new(directory)),
            note,
        },
        Err(error) => Reply::Changed {
            directory: None,
            note: format!("{note} · refresh failed ({error})"),
        },
    })
}

pub(super) fn apply_remote(
    client: &mut Client,
    plan: &Plan,
    control: &Control,
    stopped: &impl Fn() -> bool,
) -> Result<Reply> {
    if !plan
        .worker
        .as_ref()
        .is_some_and(|worker| std::ptr::eq(worker.as_ptr(), control))
    {
        return Err(io::Error::other("Cleanup review belongs to another connection").into());
    }
    let allowed = || stopped() || !control.route_allowed(&ClipboardPolicy::load());
    let note = remove_parts(plan, &allowed, |parent, path, entry| {
        remove_reviewed(client, parent, path, entry, &allowed)
    })?;
    changed(client, plan.directory(), note)
}
