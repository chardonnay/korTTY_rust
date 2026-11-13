//! One-use manual replacement of reviewed files in either sync direction.

use super::*;
use crate::sftp_window::worker::resume::durable;
use std::cell::Cell;

pub(in crate::sftp_window) mod draft;
pub(in crate::sftp_window::worker) mod metadata;
mod remote;
#[cfg(windows)]
mod scheduled;
#[cfg(windows)]
pub(in crate::sftp_window) use scheduled::ScheduledDownload;

pub(crate) struct Request {
    approval: Approval,
    review: Arc<Review>,
    roots: [(Location, DirectoryStamp); 2],
    direction: Side,
    batch: bool,
    entries: Vec<Entry>,
    combined: Option<Combined>,
}

pub(in crate::sftp_window) struct Combined {
    copy: CopyPlan,
    removals: Option<removal::Plan>,
}

#[derive(Default)]
struct Additions {
    copied: usize,
    created: usize,
    failed: usize,
}

struct Entry {
    source: Location,
    source_stamp: SnapshotEntry,
    source_parent: (Location, DirectoryStamp),
    target: Location,
    target_stamp: SnapshotEntry,
    target_parent: (Location, DirectoryStamp),
    labels: [String; 2],
    sizes: [u64; 2],
}

impl Request {
    pub(in crate::sftp_window) fn new(
        review: Arc<Review>,
        route: SshTarget,
        worker: &Worker,
        direction: Side,
        path: &Relative,
    ) -> io::Result<Self> {
        Self::build(review, route, worker, direction, Some(path))
    }

    pub(in crate::sftp_window) fn batch(
        review: Arc<Review>,
        route: SshTarget,
        worker: &Worker,
        direction: Side,
    ) -> io::Result<Self> {
        Self::build(review, route, worker, direction, None)
    }

    pub(in crate::sftp_window) fn combined(
        review: Arc<Review>,
        route: SshTarget,
        worker: &Worker,
        direction: Side,
        remove: bool,
    ) -> io::Result<Self> {
        let mut request = Self::batch(Arc::clone(&review), route, worker, direction)?;
        let copy = CopyPlan::build(
            Arc::clone(&review),
            direction,
            None,
            false,
            CopyOrigin::Browser,
        )?;
        let removals = remove
            .then(|| removal::Plan::new(&review, direction, &copy))
            .transpose()?;
        let combined = Combined { copy, removals };
        let replacements = request.entries.len();
        if combined.copy.actions.len() + combined.removal_count() + replacements > MAX_ACTIONS {
            return Err(io::Error::other(
                "Reviewed synchronization exceeds 256 entries",
            ));
        }
        let new_bytes = combined.copy.bytes();
        let changed_bytes: u64 = request.entries.iter().map(|entry| entry.sizes[0]).sum();
        if new_bytes + changed_bytes > MAX_BYTES {
            return Err(io::Error::other(
                "Reviewed synchronization exceeds 4 GiB of source data",
            ));
        }
        let path_bytes = combined.copy.roots.iter().map(String::len).sum::<usize>()
            + combined
                .copy_paths()
                .chain(combined.removal_paths())
                .map(|(_, path)| path.len())
                .sum::<usize>()
            + request
                .entries
                .iter()
                .flat_map(|entry| &entry.labels)
                .map(String::len)
                .sum::<usize>();
        if path_bytes > MAX_REVIEW_LABEL_BYTES {
            return Err(io::Error::other(
                "The exact paths exceed the synchronization review display limit",
            ));
        }
        request.combined = Some(combined);
        Ok(request)
    }

    fn build(
        review: Arc<Review>,
        route: SshTarget,
        worker: &Worker,
        direction: Side,
        selection: Option<&Relative>,
    ) -> io::Result<Self> {
        let (source, target) = review.direction(direction).ok_or_else(invalid)?;
        let roots = [source.root_directory(), target.root_directory()];
        let paths = review.replacement_candidates(direction);
        let mut totals = [0_u64; 2];
        let mut label_bytes = 0;
        let mut entries = Vec::new();
        for path in paths.filter(|path| selection.is_none_or(|selected| selected == *path)) {
            if entries.len() == MAX_ACTIONS {
                return Err(io::Error::other("Reviewed replacements exceed 256 files"));
            }
            let entry = Entry::new(source, target, path)?;
            for (total, size) in totals.iter_mut().zip(entry.sizes) {
                *total = total
                    .checked_add(size)
                    .filter(|&bytes| bytes <= MAX_BYTES)
                    .ok_or_else(|| {
                        io::Error::other("Reviewed replacements exceed 4 GiB on one side")
                    })?;
            }
            label_bytes += entry.labels.iter().map(String::len).sum::<usize>();
            if label_bytes > MAX_REVIEW_LABEL_BYTES {
                return Err(io::Error::other(
                    "The exact file paths exceed the replacement review display limit",
                ));
            }
            entries.push(entry);
        }
        if entries.is_empty() {
            return Err(invalid());
        }
        Ok(Self {
            approval: Approval::new(route, worker)?,
            review,
            roots,
            direction,
            batch: selection.is_none(),
            entries,
            combined: None,
        })
    }
}

fn invalid() -> io::Error {
    io::Error::other("The selected entry is not an eligible replacement")
}

impl Entry {
    fn new(
        source: &crate::sftp_window::tree::snapshot::Manifest,
        target: &crate::sftp_window::tree::snapshot::Manifest,
        path: &Relative,
    ) -> io::Result<Self> {
        let source_stamp = source.entries.get(path).ok_or_else(invalid)?.clone();
        let target_stamp = target.entries.get(path).ok_or_else(invalid)?.clone();
        let (source_size, _) = source_stamp.file_metadata().ok_or_else(invalid)?;
        let (target_size, _) = target_stamp.file_metadata().ok_or_else(invalid)?;
        let source_parent = source.parent(path).ok_or_else(invalid)?;
        let target_parent = target.parent(path).ok_or_else(invalid)?;
        let source = source.location(path)?;
        let target = target.location(path)?;
        let labels = [review_root(&source)?, review_root(&target)?];
        Ok(Self {
            source,
            source_stamp,
            source_parent,
            target,
            target_stamp,
            target_parent,
            labels,
            sizes: [source_size, target_size],
        })
    }
}

pub struct Plan {
    approval: Approval,
    review: Option<Arc<Review>>,
    roots: [(Location, DirectoryStamp); 2],
    direction: Side,
    entries: Vec<EntryPlan>,
    omitted: Vec<String>,
    combined: Option<Combined>,
}

pub(in crate::sftp_window) struct EntryPlan {
    entry: Entry,
    digests: [[u8; 32]; 2],
    digest_labels: [String; 2],
    metadata: Metadata,
}

enum Metadata {
    Local(metadata::Metadata),
    Remote,
}

#[derive(Debug)]
struct Completion {
    backup: String,
    warning: Option<Failure>,
}

type ReplaceResult = std::result::Result<Completion, ReplaceFailure>;

#[derive(Clone, Copy)]
struct Recovery<'a> {
    control: &'a Control,
    snapshot: &'a (u64, durable::Context),
}

#[derive(Clone, Copy)]
struct ReplaceScope<'a> {
    approval: &'a Approval,
    recovery: Option<Recovery<'a>>,
}

#[derive(Clone, Copy)]
struct LocalReplace<'a> {
    metadata: &'a metadata::Metadata,
    strict_source: bool,
    backup_prefix: Option<&'a str>,
}

#[derive(Debug)]
struct ReplaceFailure {
    error: Failure,
    uncertain: bool,
}

impl ReplaceFailure {
    fn uncertain(error: Failure) -> Self {
        Self {
            error,
            uncertain: true,
        }
    }
}

impl From<Failure> for ReplaceFailure {
    fn from(error: Failure) -> Self {
        Self {
            error,
            uncertain: false,
        }
    }
}

impl From<io::Error> for ReplaceFailure {
    fn from(error: io::Error) -> Self {
        Failure::from(error).into()
    }
}

impl From<Error> for ReplaceFailure {
    fn from(error: Error) -> Self {
        Failure::from(error).into()
    }
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SyncReplacement")
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn applies_to(&self, worker: &Worker, route: &SshTarget) -> bool {
        self.approval.applies_to(worker, route)
    }

    pub(in crate::sftp_window) fn direction(&self) -> Side {
        self.direction
    }

    pub(in crate::sftp_window) fn is_batch(&self) -> bool {
        self.review.is_none()
    }

    pub(in crate::sftp_window) fn entries(&self) -> &[EntryPlan] {
        &self.entries
    }

    pub(in crate::sftp_window) fn omitted(&self) -> &[String] {
        &self.omitted
    }

    pub(in crate::sftp_window) fn combined(&self) -> Option<&Combined> {
        self.combined.as_ref()
    }

    pub(in crate::sftp_window) fn bytes(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.entry.sizes[0])
            .sum::<u64>()
            + self
                .combined
                .as_ref()
                .map_or(0, |combined| combined.copy.bytes())
    }

    #[cfg(test)]
    pub(in crate::sftp_window::worker) fn replace(
        &self,
        client: &mut Client,
        progress: &Progress,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> Result<()>,
    ) -> Result<String> {
        self.entries[0]
            .replace(
                client,
                progress,
                self.is_batch(),
                stopped,
                authorize,
                ReplaceScope {
                    approval: &self.approval,
                    recovery: None,
                },
            )
            .map(|completion| completion.backup)
            .map_err(|failure| failure.error)
    }
}

impl EntryPlan {
    pub(in crate::sftp_window) fn labels(&self) -> [&str; 2] {
        self.entry.labels.each_ref().map(String::as_str)
    }

    pub(in crate::sftp_window) fn sizes(&self) -> [u64; 2] {
        self.entry.sizes
    }

    pub(in crate::sftp_window) fn digests(&self) -> [&str; 2] {
        self.digest_labels.each_ref().map(String::as_str)
    }

    pub(in crate::sftp_window) fn is_in_place(&self) -> bool {
        matches!(self.metadata, Metadata::Remote) && remote::in_place(&self.entry)
    }
}

impl Combined {
    pub(in crate::sftp_window) fn copy_counts(&self) -> (usize, usize) {
        self.copy.counts()
    }

    pub(in crate::sftp_window) fn copy_paths(&self) -> impl Iterator<Item = (bool, &str)> {
        self.copy.paths()
    }

    pub(in crate::sftp_window) fn removal_paths(&self) -> impl Iterator<Item = (bool, &str)> {
        self.removals.iter().flat_map(removal::Plan::paths)
    }

    pub(in crate::sftp_window) fn removal_count(&self) -> usize {
        self.removals.as_ref().map_or(0, removal::Plan::len)
    }

    fn prepare(
        &mut self,
        client: &mut Client,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.copy.verify_destination(client)?;
        for action in &mut self.copy.actions {
            authorize()?;
            if let ActionKind::File {
                source,
                stamp,
                parent,
                digest,
            } = &mut action.kind
            {
                let current = parent.1.verify(client, &parent.0, false)?;
                *digest = Some(
                    source_digest(client, source, stamp, &(parent.0.clone(), current), stopped)
                        .map_err(|error| error.context(format!(" · {}", action.label)))?,
                );
            }
        }
        Ok(())
    }

    fn apply_copies(
        &self,
        client: &mut Client,
        progress: &Progress,
        checks: &WriteChecks<'_>,
        additions: &mut Additions,
        outcomes: &mut Vec<String>,
    ) -> Result<()> {
        self.copy.verify_destination(client)?;
        let mut parents = BTreeMap::<Option<Relative>, DirectoryStamp>::new();
        for action in &self.copy.actions {
            checks.before_write(client)?;
            let result = (|| -> Result<Option<Failure>> {
                if let Some(stamp @ DirectoryStamp::Remote(_)) = parents
                    .get(&action.parent.path)
                    .or(action.parent.reviewed.as_ref())
                {
                    let current = stamp.verify(client, &action.parent.location, false)?;
                    parents.insert(action.parent.path.clone(), current);
                }
                apply_action(client, action, progress, checks, &mut parents)
            })();
            match result {
                Ok(warning) => {
                    if action.creates_folder() {
                        additions.created += 1;
                    } else {
                        additions.copied += 1;
                    }
                    if let Some(error) = warning {
                        return Err(error.context(format!(" · {}", action.label)));
                    }
                }
                Err(error) => {
                    additions.failed += 1;
                    let error = error.context(format!(" · {}", action.label));
                    if error.disconnected {
                        return Err(error);
                    }
                    outcomes.push(error.message);
                }
            }
        }
        Ok(())
    }
}

pub(in crate::sftp_window::worker) fn prepare(
    client: &mut Client,
    request: Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<Plan> {
    let stopped = || cancel.stopped() || control.is_closed();
    let result = (|| -> Result<Plan> {
        let Request {
            approval,
            review,
            roots,
            direction,
            batch,
            entries,
            mut combined,
        } = request;
        let authorize = || approval.authorize(control, &stopped, true);
        authorize()?;
        if batch {
            verify_roots(client, &roots)?;
        } else {
            review.verify_current(client, stopped)?;
        }
        if let Some(combined) = &mut combined {
            combined.prepare(client, &stopped, &authorize)?;
        }
        let mut prepared = Vec::with_capacity(entries.len());
        let mut omitted = Vec::new();
        for entry in entries {
            authorize()?;
            let label = entry.labels[1].clone();
            match entry.prepare(client, batch, &stopped) {
                Ok(entry) => prepared.push(entry),
                Err(error) if batch && !error.disconnected => {
                    authorize()?;
                    omitted.push(format!("{label} · {}", error.message));
                }
                Err(error) => return Err(error),
            }
        }
        authorize()?;
        if prepared.is_empty() && combined.is_none() {
            return Err(io::Error::other(format!(
                "No changed files could be reviewed · {}",
                omitted.join("\n")
            ))
            .into());
        }
        Ok(Plan {
            approval,
            review: (!batch).then_some(review),
            roots,
            direction,
            entries: prepared,
            omitted,
            combined,
        })
    })();
    cancel.finish();
    result
}

fn verify_roots(client: &mut Client, roots: &[(Location, DirectoryStamp); 2]) -> Result<()> {
    for (location, stamp) in roots {
        stamp.verify(client, location, false)?;
    }
    Ok(())
}

pub(in crate::sftp_window::worker) fn apply(
    client: &mut Client,
    plan: &Plan,
    progress: &Progress,
    cancel: &Cancel,
    control: &Control,
    remove: bool,
) -> Report {
    let stopped = || cancel.stopped() || control.is_closed();
    let authorize = || plan.approval.authorize(control, &stopped, true);
    let snapshot = durable::snapshot(control).filter(|(_, context)| {
        context.route == plan.approval.route && context.host_key == plan.approval.peer
    });
    let scope = ReplaceScope {
        approval: &plan.approval,
        recovery: snapshot
            .as_ref()
            .map(|snapshot| Recovery { control, snapshot }),
    };
    let mut completed = 0;
    let mut failed = 0;
    let mut additions = Additions::default();
    let mut removed = removal::Counts::default();
    let mut outcomes = Vec::new();
    let removals = plan
        .combined
        .as_ref()
        .and_then(|combined| combined.removals.as_ref())
        .filter(|_| remove);
    let mut started_removals = false;
    let result = (|| -> Result<()> {
        authorize()?;
        if remove && removals.is_none() {
            return Err(io::Error::other("The confirmed removal review is unavailable").into());
        }
        plan.approval.claim()?;
        if let Some(review) = &plan.review {
            review.verify_current(client, stopped)?;
        } else {
            verify_roots(client, &plan.roots)?;
        }
        if let Some(removals) = removals {
            removals.verify_destination(client, &stopped)?;
        }
        if let Some(combined) = &plan.combined {
            let checks = WriteChecks {
                cancelled: &stopped,
                disconnected: &|| control.is_closed(),
                standing: None,
                combined: Some(&authorize),
                scheduled: None,
                origin: CopyOrigin::Browser,
            };
            combined.apply_copies(client, progress, &checks, &mut additions, &mut outcomes)?;
        }
        for entry in &plan.entries {
            authorize()?;
            let label = &entry.entry.labels[1];
            match entry.replace(
                client,
                progress,
                plan.is_batch(),
                &stopped,
                &authorize,
                scope,
            ) {
                Ok(completion) => {
                    completed += 1;
                    if plan.is_batch() {
                        outcomes.push(format!(
                            "{label} · Replaced · Original saved at {}",
                            completion.backup
                        ));
                    } else {
                        let side = if plan.direction == Side::Local {
                            "local"
                        } else {
                            "server"
                        };
                        outcomes.push(format!(
                            "Replaced the reviewed {side} file · Original saved at {}",
                            completion.backup
                        ));
                    }
                    if let Some(warning) = completion.warning {
                        if warning.disconnected {
                            return Err(warning.context(format!(" · {label}")));
                        }
                        outcomes.push(format!("{label} · {}", warning.message));
                    }
                }
                Err(failure) => {
                    failed += 1;
                    if !plan.is_batch() {
                        return Err(failure.error);
                    }
                    if failure.uncertain || failure.error.disconnected {
                        return Err(failure.error.context(format!(" · {label}")));
                    }
                    outcomes.push(format!("{label} · {}", failure.error.message));
                }
            }
        }
        if let Some(removals) = removals
            && additions.failed == 0
            && failed == 0
        {
            started_removals = true;
            removals.apply(client, &authorize, &stopped, &mut removed)?;
        }
        Ok(())
    })();
    let mut disconnected = control.is_closed();
    if let Err(error) = result {
        disconnected |= error.disconnected;
        let action = if cancel.stopped() {
            "cancelled"
        } else {
            "stopped"
        };
        let operation = if plan.combined.is_some() {
            "synchronization"
        } else {
            "replacement"
        };
        outcomes.push(format!("Reviewed {operation} {action} · {}", error.message));
    }
    if let Some(combined) = &plan.combined {
        let (files, folders) = combined.copy_counts();
        let pending = files + folders + plan.entries.len()
            - additions.copied
            - additions.created
            - additions.failed
            - completed
            - failed;
        let removal_note = if let Some(removals) = removals {
            if started_removals {
                removed.note(removals.len())
            } else {
                format!(" · {} confirmed removals not attempted", removals.len())
            }
        } else {
            String::new()
        };
        outcomes.insert(0, format!(
            "Copied {} of {files} new files · Created {} of {folders} folders · Replaced {completed} of {} reviewed files · {} failed · {pending} not attempted{removal_note}",
            additions.copied,
            additions.created,
            plan.entries.len(),
            additions.failed + failed,
        ));
    } else if plan.is_batch() {
        let pending = plan.entries.len() - completed - failed;
        outcomes.insert(0, format!(
            "Replaced {completed} of {} reviewed files · {failed} failed · {pending} not attempted",
            plan.entries.len()
        ));
    }
    let mut note = outcomes.join("\n");
    let root = &plan.roots[1].0;
    let directory = if disconnected {
        None
    } else {
        match list(client, root) {
            Ok(directory) if &directory.path == root => Some(directory),
            Ok(_) => {
                note.push_str(" · The destination folder changed before refresh");
                None
            }
            Err(error) => {
                disconnected |= error.disconnected;
                note.push_str(&format!(
                    " · Destination refresh failed ({})",
                    error.message
                ));
                None
            }
        }
    };
    cancel.finish();
    Report {
        note,
        disconnected,
        directory,
    }
}

impl EntryPlan {
    fn replace(
        &self,
        client: &mut Client,
        progress: &Progress,
        batch: bool,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> Result<()>,
        scope: ReplaceScope<'_>,
    ) -> ReplaceResult {
        match &self.metadata {
            Metadata::Local(metadata) => self.replace_local(
                client,
                progress,
                stopped,
                authorize,
                LocalReplace {
                    metadata,
                    strict_source: !batch,
                    backup_prefix: None,
                },
            ),
            Metadata::Remote => remote::replace(
                client,
                &self.entry,
                self.digests,
                progress,
                stopped,
                authorize,
                scope,
            ),
        }
    }

    fn replace_local(
        &self,
        client: &mut Client,
        progress: &Progress,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> Result<()>,
        options: LocalReplace<'_>,
    ) -> ReplaceResult {
        let LocalReplace {
            metadata,
            strict_source,
            backup_prefix,
        } = options;
        let request = &self.entry;
        let (Location::Remote(source), SnapshotEntry::Remote(source_stamp)) =
            (&request.source, &request.source_stamp)
        else {
            return Err(io::Error::other("The reviewed server source is invalid").into());
        };
        let (Location::Local(target), (Location::Local(folder), DirectoryStamp::Local(parent))) =
            (&request.target, &request.target_parent)
        else {
            return Err(io::Error::other("The reviewed local destination is invalid").into());
        };
        let name = target
            .file_name()
            .filter(|_| target.parent() == Some(folder.as_path()))
            .ok_or_else(|| io::Error::other("The reviewed local destination name is invalid"))?;
        let (source_folder, source_parent) = &request.source_parent;
        source_parent.verify(client, source_folder, strict_source)?;
        authorize()?;
        let original = request.open_local_target()?;
        metadata.require_matches(original.file())?;
        drop(original);
        let staged = StagedDownload::stage_for_replacement(target, parent)?;
        let file = staged.writer()?;
        let mut digest = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        let output = ObserveOutput(Counted(progress, file), |bytes: &[u8]| digest.update(bytes));
        client.download_regular(source, *source_stamp, CheckedOutput(output, stopped))?;
        if hash_bytes(digest.finalize()) != self.digests[0] {
            return Err(changed_bytes().into());
        }
        let modified = source_stamp
            .modified
            .ok_or_else(|| io::Error::other("The reviewed modification time is missing"))?;
        file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(modified)))?;
        file.sync_all()?;
        let file_stamp = FileStamp::read(file)?;
        staged.verify_stage(&file_stamp)?;
        source_parent.verify(client, source_folder, strict_source)?;
        authorize()?;
        let might_have_moved = Cell::new(false);
        let before_move = |file: &File| {
            authorize()?;
            metadata.require_matches(file)?;
            might_have_moved.set(true);
            Ok(())
        };
        let backup = removal::local::quarantine_for_replacement(
            folder,
            name,
            parent,
            (&request.target_stamp, self.digests[1]),
            &before_move,
            stopped,
            backup_prefix,
        )
        .map_err(|error| {
            if might_have_moved.get() {
                ReplaceFailure::uncertain(error)
            } else {
                error.into()
            }
        })?;
        let publish = (|| -> Result<()> {
            staged.verify_partial(&file_stamp)?;
            backup.verify()?;
            metadata.require_matches(backup.file())?;
            staged.publish_checked(|file| {
                authorize().map_err(|error| io::Error::other(error.message))?;
                metadata.require_matches(backup.file())?;
                metadata.apply_to(file)?;
                file.sync_all()?;
                metadata.require_matches(backup.file())?;
                authorize().map_err(|error| io::Error::other(error.message))
            })?;
            Ok(())
        })();
        match publish {
            Ok(()) => Ok(Completion {
                backup: backup_label(&backup.retain()),
                warning: None,
            }),
            Err(error) => {
                let path = backup_label(backup.path());
                match backup.restore() {
                    Ok(()) => Err(error.context(" · The original file was restored").into()),
                    Err(restore) => Err(ReplaceFailure::uncertain(error.context(format!(
                        " · Restore or backup cleanup failed ({}) · Recovery path {path}",
                        restore.message
                    )))),
                }
            }
        }
    }
}

impl Entry {
    fn prepare(
        mut self,
        client: &mut Client,
        batch: bool,
        stopped: &impl Fn() -> bool,
    ) -> Result<EntryPlan> {
        if batch {
            for (path, stamp) in [&mut self.source_parent, &mut self.target_parent] {
                *stamp = stamp.verify(client, path, false)?;
            }
        }
        let source = source_digest(
            client,
            &self.source,
            &self.source_stamp,
            &self.source_parent,
            stopped,
        )?;
        let local = match &self.target {
            Location::Local(_) => {
                let original = self.open_local_target()?;
                let metadata = metadata::Metadata::capture(original.file())?;
                Some((original, metadata))
            }
            Location::Remote(_) => {
                remote::prepare(client, &self, stopped)?;
                None
            }
        };
        let target = source_digest(
            client,
            &self.target,
            &self.target_stamp,
            &self.target_parent,
            stopped,
        )?;
        let metadata = match local {
            Some((original, metadata)) => {
                self.verify_local_target(&original)?;
                metadata.require_matches(original.file())?;
                Metadata::Local(metadata)
            }
            None => Metadata::Remote,
        };
        let digests = [source, target];
        Ok(EntryPlan {
            entry: self,
            digests,
            digest_labels: digests.map(digest_label),
            metadata,
        })
    }

    fn open_local_target(&self) -> Result<LocalSource> {
        let Location::Local(path) = &self.target else {
            return Err(io::Error::other("The reviewed local destination is invalid").into());
        };
        let original = LocalSource::open(path)?;
        self.verify_local_target(&original)?;
        crate::private_files::require_single_link(original.file())?;
        Ok(original)
    }

    fn verify_local_target(&self, original: &LocalSource) -> Result<()> {
        let (SnapshotEntry::LocalFile { stamp, identity }, (_, DirectoryStamp::Local(parent))) =
            (&self.target_stamp, &self.target_parent)
        else {
            return Err(io::Error::other("The reviewed local destination is invalid").into());
        };
        Ok(original.verify_reviewed(stamp, identity, parent)?)
    }
}

fn digest_label(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn backup_label(path: &std::path::Path) -> String {
    path.as_os_str()
        .as_encoded_bytes()
        .escape_ascii()
        .to_string()
}
