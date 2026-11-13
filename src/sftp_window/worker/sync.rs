//! Applies an explicitly reviewed cross-side copy plan using no-replace publication.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::sync::{Condvar, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use kortty_ssh::sftp::UploadOwner;
use pgp::crypto::hash::HashAlgorithm;

use crate::private_files::FileStamp;
use crate::sftp_window::compare::folders::Review;
use crate::sftp_window::tree::snapshot::{DirectoryStamp, Entry as SnapshotEntry, Kind, Relative};
use crate::sftp_window::{LocalSource, Side, StagedDownload, local_tree};
use crate::workspace::SshTarget;

use super::*;

use super::download_access;
mod removal;
pub(in crate::sftp_window) mod replacement;

const MAX_ACTIONS: usize = 256;
const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub(crate) const MAX_STANDING_ACTIONS: usize = MAX_ACTIONS;
pub(crate) const MAX_STANDING_BYTES: u64 = 256 * 1024 * 1024;
const MAX_LABEL_BYTES: usize = 4096;
const MAX_REVIEW_LABEL_BYTES: usize = 256 * 1024;
const CANCEL_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
pub(crate) struct Cancel(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    requested: AtomicBool,
    finished: Mutex<bool>,
    complete: Condvar,
}

impl Cancel {
    pub(in crate::sftp_window) fn stop_local(&self) {
        self.0.requested.store(true, Ordering::Release);
    }

    pub(in crate::sftp_window) fn stop(&self, worker: &Worker) {
        if self.0.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let cancel = self.clone();
        let owner = Arc::downgrade(&worker.control);
        if thread::Builder::new()
            .name("kortty-sftp-cancel".into())
            .spawn(move || {
                let finished = cancel
                    .0
                    .finished
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let (finished, _) = cancel
                    .0
                    .complete
                    .wait_timeout_while(finished, CANCEL_GRACE, |finished| !*finished)
                    .unwrap_or_else(|error| error.into_inner());
                if !*finished && let Some(control) = owner.upgrade() {
                    control.abort();
                }
            })
            .is_err()
        {
            worker.abort();
        }
    }

    pub(super) fn stopped(&self) -> bool {
        self.0.requested.load(Ordering::Acquire)
    }

    pub(super) fn finish(&self) {
        let mut finished = self
            .0
            .finished
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *finished = true;
        self.0.complete.notify_all();
    }
}

pub(crate) struct Plan {
    copy: CopyPlan,
    removals: Option<removal::Plan>,
    approval: Approval,
}

struct Approval {
    route: SshTarget,
    peer: HostKey,
    owner: Weak<Control>,
    used: AtomicBool,
}

impl std::ops::Deref for Plan {
    type Target = CopyPlan;

    fn deref(&self) -> &Self::Target {
        &self.copy
    }
}

pub(crate) struct CopyPlan {
    review: Arc<Review>,
    roots: [String; 2],
    standing_roots: Option<StandingRoots>,
    actions: Vec<Action>,
    files: usize,
    bytes: u64,
    origin: CopyOrigin,
    #[cfg(windows)]
    replacements: Option<scheduled_replacement::Replacements>,
}

#[cfg(windows)]
mod scheduled_replacement;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReplacementPreview {
    pub(crate) source: String,
    pub(crate) target: String,
    pub(crate) source_sha256: String,
    pub(crate) target_sha256: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CopyOrigin {
    Browser,
    Scheduler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StandingLimits {
    pub(crate) actions: usize,
    pub(crate) bytes: u64,
}

impl StandingLimits {
    pub(crate) fn valid(self) -> bool {
        (1..=MAX_STANDING_ACTIONS).contains(&self.actions)
            && (1..=MAX_STANDING_BYTES).contains(&self.bytes)
    }
}

struct StandingRoots {
    roots: [(Location, DirectoryStamp); 2],
    binding: [u8; 32],
}

impl StandingRoots {
    fn new(roots: [(Location, DirectoryStamp); 2]) -> io::Result<Self> {
        let [
            (Location::Local(local), DirectoryStamp::Local(identity)),
            (Location::Remote(remote), DirectoryStamp::Remote(attributes)),
        ] = &roots
        else {
            return Err(io::Error::other(
                "The standing copy folders could not be verified",
            ));
        };
        let (Some(uid), Some(gid), Some(mode)) =
            (attributes.uid, attributes.gid, attributes.permissions)
        else {
            return Err(io::Error::other(
                "Standing copies require known ownership and permissions for the server folder",
            ));
        };
        // Directory sizes and timestamps change as entries arrive. SFTP v3 supplies
        // no stable file ID, so retain the canonical path and ownership attributes.
        let bytes = serde_json::to_vec(&(
            "kortty-standing-sftp-roots-v1",
            local.as_os_str().as_encoded_bytes(),
            identity,
            remote,
            uid,
            gid,
            mode,
        ))?;
        let binding = hash_bytes(
            HashAlgorithm::Sha256
                .digest(&bytes)
                .map_err(io::Error::other)?
                .into_boxed_slice(),
        );
        Ok(Self { roots, binding })
    }

    fn verify(&self, client: &mut Client, strict: bool) -> Result<()> {
        for (location, stamp) in &self.roots {
            stamp.verify(client, location, strict)?;
        }
        Ok(())
    }
}

struct Action {
    label: String,
    path: Relative,
    target: Location,
    parent: Parent,
    kind: ActionKind,
}

struct Parent {
    location: Location,
    path: Option<Relative>,
    reviewed: Option<DirectoryStamp>,
}

enum ActionKind {
    Folder(Location, DirectoryStamp),
    File {
        source: Location,
        stamp: SnapshotEntry,
        parent: (Location, DirectoryStamp),
        digest: Option<[u8; 32]>,
    },
}

impl Action {
    fn creates_folder(&self) -> bool {
        matches!(&self.kind, ActionKind::Folder(..))
    }
}

impl Plan {
    pub(in crate::sftp_window) fn new_sync(
        review: Arc<Review>,
        route: SshTarget,
        worker: &Worker,
        to: Side,
        remove: bool,
    ) -> io::Result<Self> {
        let can_review_existing = review.destination_only(to).next().is_some()
            || review.replacement_candidates(to).next().is_some();
        let copy = CopyPlan::build(
            Arc::clone(&review),
            to,
            None,
            can_review_existing,
            CopyOrigin::Browser,
        )?;
        let removals = if remove {
            Some(removal::Plan::new(&review, to, &copy)?)
        } else {
            None
        };
        Ok(Self {
            copy,
            removals,
            approval: Approval::new(route, worker)?,
        })
    }

    pub(in crate::sftp_window) fn removal_paths(&self) -> impl Iterator<Item = (bool, &str)> {
        self.removals.iter().flat_map(removal::Plan::paths)
    }

    pub(in crate::sftp_window) fn removal_count(&self) -> usize {
        self.removals.as_ref().map_or(0, removal::Plan::len)
    }

    pub(in crate::sftp_window) fn applies_to(&self, worker: &Worker, route: &SshTarget) -> bool {
        self.approval.applies_to(worker, route)
    }
}

impl Approval {
    fn new(route: SshTarget, worker: &Worker) -> io::Result<Self> {
        let peer = worker
            .verified_host_key()
            .filter(|_| !worker.is_closed())
            .cloned()
            .ok_or_else(|| io::Error::other("A verified live SSH connection is required"))?;
        Ok(Self {
            route,
            peer,
            owner: Arc::downgrade(&worker.control),
            used: AtomicBool::new(false),
        })
    }

    fn applies_to(&self, worker: &Worker, route: &SshTarget) -> bool {
        self.route == *route && self.bound(&worker.control) && !self.used.load(Ordering::Acquire)
    }

    fn bound(&self, control: &Control) -> bool {
        std::ptr::eq(self.owner.as_ptr(), control)
            && control.host_key.as_ref() == Some(&self.peer)
            && !control.is_closed()
    }

    fn authorize(
        &self,
        control: &Control,
        stopped: &impl Fn() -> bool,
        enforce_policy: bool,
    ) -> Result<()> {
        require_running(stopped)?;
        if !self.bound(control) {
            return Err(io::Error::other("The reviewed SSH connection changed").into());
        }
        if enforce_policy {
            let policy = ClipboardPolicy::load();
            if policy.problem.is_some()
                || !policy.file_transfer_allowed
                || !self.route.workflow_route_allowed(&policy)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Current policy does not allow this reviewed synchronization",
                )
                .into());
            }
        }
        Ok(())
    }

    fn claim(&self) -> Result<()> {
        if self.used.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("This synchronization review was already used").into());
        }
        Ok(())
    }
}

impl CopyPlan {
    pub(in crate::sftp_window) fn new(review: Arc<Review>, to: Side) -> io::Result<Self> {
        Self::build(review, to, None, false, CopyOrigin::Scheduler)
    }

    pub(in crate::sftp_window) fn new_replacing(review: Arc<Review>, to: Side) -> io::Result<Self> {
        #[cfg(windows)]
        if to == Side::Local {
            let mut plan = Self::build(review, to, None, true, CopyOrigin::Scheduler)?;
            plan.replacements = Some(scheduled_replacement::Replacements::default());
            return Ok(plan);
        }
        let _ = (review, to);
        Err(io::Error::other(
            "Reviewed scheduled replacements require a Windows download destination",
        ))
    }

    pub(in crate::sftp_window) fn new_standing(
        review: Arc<Review>,
        to: Side,
        limits: StandingLimits,
    ) -> io::Result<Self> {
        Self::build(review, to, Some(limits), false, CopyOrigin::Scheduler)
    }

    fn build(
        review: Arc<Review>,
        to: Side,
        standing: Option<StandingLimits>,
        allow_empty: bool,
        origin: CopyOrigin,
    ) -> io::Result<Self> {
        let (source, target) = review.direction(to).ok_or_else(|| {
            io::Error::other("Directional copy requires a local and a server folder")
        })?;
        let roots = [review_root(&source.root)?, review_root(&target.root)?];
        let standing_roots = if standing.is_some() {
            let (local, remote) = match to {
                Side::Local => (target, source),
                Side::Remote => (source, target),
            };
            Some(StandingRoots::new([
                local.root_directory(),
                remote.root_directory(),
            ])?)
        } else {
            None
        };
        let max_actions = standing.map_or(MAX_ACTIONS, |limits| limits.actions);
        let max_bytes = standing.map_or(MAX_BYTES, |limits| limits.bytes);
        let mut actions = Vec::new();
        let mut new_folders = BTreeSet::<Relative>::new();
        let mut files = 0;
        let mut bytes = 0u64;
        let mut label_bytes = roots.iter().map(String::len).sum::<usize>();
        for path in review.source_only(to) {
            let stamp = source
                .entries
                .get(path)
                .ok_or_else(|| io::Error::other("A reviewed source entry is missing"))?;
            let parent_path = path.parent();
            let (location, reviewed) = if let Some((location, stamp)) = target.parent(path) {
                (location, Some(stamp))
            } else if let Some(parent) = parent_path
                .as_ref()
                .filter(|parent| new_folders.contains(*parent))
            {
                (target.location(parent)?, None)
            } else {
                return Err(io::Error::other(
                    "A reviewed destination folder is unavailable",
                ));
            };
            let kind = match stamp.kind() {
                Kind::Folder => {
                    let (location, stamp) = source.directory(path).ok_or_else(|| {
                        io::Error::other("A reviewed source folder could not be verified")
                    })?;
                    new_folders.insert(path.clone());
                    ActionKind::Folder(location, stamp)
                }
                Kind::File => {
                    let (size, modified) = stamp
                        .file_metadata()
                        .ok_or_else(|| io::Error::other("A source file has incomplete metadata"))?;
                    if to == Side::Remote {
                        wire_modified(modified)?;
                    }
                    bytes = bytes
                        .checked_add(size)
                        .filter(|&sum| sum <= max_bytes)
                        .ok_or_else(|| {
                            io::Error::other(if standing.is_some() {
                                "Standing copy exceeds the per-firing byte limit"
                            } else {
                                "Reviewed copy exceeds 4 GiB"
                            })
                        })?;
                    files += 1;
                    ActionKind::File {
                        source: source.location(path)?,
                        stamp: stamp.clone(),
                        parent: source.parent(path).ok_or_else(|| {
                            io::Error::other("A source folder could not be verified")
                        })?,
                        digest: None,
                    }
                }
                Kind::Blocked => continue,
            };
            if actions.len() == max_actions {
                return Err(io::Error::other(if standing.is_some() {
                    "Standing copy exceeds the per-firing file and folder limit"
                } else {
                    "Reviewed copy exceeds 256 files and folders"
                }));
            }
            let label = path.review_label();
            label_bytes += label.len();
            if label.len() > MAX_LABEL_BYTES || label_bytes > MAX_REVIEW_LABEL_BYTES {
                return Err(io::Error::other(
                    "The exact file paths exceed the copy review display limit",
                ));
            }
            actions.push(Action {
                label,
                path: path.clone(),
                target: target.location(path)?,
                parent: Parent {
                    location,
                    path: parent_path,
                    reviewed,
                },
                kind,
            });
        }
        if actions.is_empty() && standing.is_none() && !allow_empty {
            return Err(io::Error::other(
                "There are no source-only regular files or folders to copy",
            ));
        }
        Ok(Self {
            review,
            roots,
            standing_roots,
            actions,
            files,
            bytes,
            origin,
            #[cfg(windows)]
            replacements: None,
        })
    }

    pub(in crate::sftp_window) fn standing_binding(&self) -> Option<[u8; 32]> {
        self.standing_roots.as_ref().map(|roots| roots.binding)
    }

    pub(in crate::sftp_window) fn roots(&self) -> &[String; 2] {
        &self.roots
    }

    pub(in crate::sftp_window) fn paths(&self) -> impl Iterator<Item = (bool, &str)> {
        self.actions
            .iter()
            .map(|action| (action.creates_folder(), action.label.as_str()))
    }

    pub(in crate::sftp_window) fn bytes(&self) -> u64 {
        #[cfg(windows)]
        if let Some(replacements) = &self.replacements {
            return self.bytes + replacements.bytes;
        }
        self.bytes
    }

    pub(in crate::sftp_window) fn replacement_preview(&self) -> Vec<ReplacementPreview> {
        #[cfg(windows)]
        if let Some(replacements) = &self.replacements {
            return replacements.preview();
        }
        Vec::new()
    }

    pub(in crate::sftp_window) fn counts(&self) -> (usize, usize) {
        (self.files, self.actions.len() - self.files)
    }

    fn verify_destination(&self, client: &mut Client) -> Result<()> {
        if self
            .actions
            .iter()
            .any(|action| !action.creates_folder() && matches!(&action.target, Location::Remote(_)))
            && !client.supports_hardlink()
        {
            return Err(io::Error::other(
                "Reviewed server copies require the SFTP hardlink extension for atomic publication",
            )
            .into());
        }
        for action in &self.actions {
            if let Some(DirectoryStamp::Remote(parent)) = &action.parent.reviewed {
                require_private_parent(*parent)?;
            }
        }
        Ok(())
    }

    pub(in crate::sftp_window) fn prove(
        &mut self,
        client: &mut Client,
        cancelled: &impl Fn() -> bool,
    ) -> Result<[u8; 32]> {
        if self.bytes > MAX_STANDING_BYTES {
            return Err(io::Error::other("Scheduled copy review exceeds 256 MiB").into());
        }
        self.verify_destination(client)?;
        let mut proof = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        {
            let mut field = |data: &[u8]| {
                proof.update(&(data.len() as u64).to_be_bytes());
                proof.update(data);
            };
            field(b"kortty-reviewed-sftp-v1");
            for root in &self.roots {
                field(root.as_bytes());
            }
            self.review.review_bytes(&mut field)?;
            for action in &mut self.actions {
                require_running(cancelled)?;
                field(action.label.as_bytes());
                field(&[u8::from(action.creates_folder())]);
                if let ActionKind::File {
                    source,
                    stamp,
                    parent,
                    digest,
                } = &mut action.kind
                {
                    let hash = source_digest(client, source, stamp, parent, cancelled)?;
                    field(&hash);
                    *digest = Some(hash);
                }
            }
            #[cfg(windows)]
            if let Some(replacements) = &mut self.replacements {
                field(b"kortty-reviewed-sftp-windows-replacement-v1");
                replacements.prepare(
                    client,
                    &self.review,
                    self.actions.len(),
                    self.bytes,
                    &mut field,
                    cancelled,
                )?;
            }
        }
        self.review.verify_current(client, cancelled)?;
        Ok(hash_bytes(proof.finalize()))
    }
}

fn source_digest(
    client: &mut Client,
    source: &Location,
    stamp: &SnapshotEntry,
    (folder, parent): &(Location, DirectoryStamp),
    cancelled: &impl Fn() -> bool,
) -> Result<[u8; 32]> {
    let mut digest = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    parent.verify(client, folder, true)?;
    let sink = ObserveOutput(io::sink(), |bytes: &[u8]| digest.update(bytes));
    let mut output = CheckedOutput(sink, cancelled);
    match (source, stamp, parent) {
        (
            Location::Local(path),
            SnapshotEntry::LocalFile { stamp, identity },
            DirectoryStamp::Local(parent),
        ) => {
            let file = LocalSource::open(path)?;
            file.verify_reviewed(stamp, identity, parent)?;
            let count = io::copy(&mut file.file().take(file.size() + 1), &mut output)?;
            if count != file.size() {
                return Err(changed_bytes().into());
            }
            file.verify_reviewed(stamp, identity, parent)?;
        }
        (Location::Remote(path), SnapshotEntry::Remote(stamp), DirectoryStamp::Remote(_)) => {
            client.download_regular(path, *stamp, output)?;
        }
        _ => return Err(io::Error::other("The reviewed source is invalid").into()),
    }
    parent.verify(client, folder, true)?;
    Ok(hash_bytes(digest.finalize()))
}

fn hash_bytes(bytes: Box<[u8]>) -> [u8; 32] {
    bytes
        .as_ref()
        .try_into()
        .expect("SHA-256 produces 32 bytes")
}

fn changed_bytes() -> io::Error {
    io::Error::other("The source content changed after review")
}

pub(in crate::sftp_window) fn review_root(path: &Location) -> io::Result<String> {
    let (place, bytes) = match path {
        Location::Local(path) => ("Local", path.as_os_str().as_encoded_bytes()),
        Location::Remote(path) => ("Server", path.as_slice()),
    };
    let too_long = || io::Error::other("An exact root path exceeds the copy review display limit");
    if bytes.len() > MAX_LABEL_BYTES {
        return Err(too_long());
    }
    let label = bytes.escape_ascii().to_string();
    if label.len() > MAX_LABEL_BYTES {
        return Err(too_long());
    }
    Ok(format!("{place} · {label}"))
}

fn wire_modified(modified: SystemTime) -> io::Result<u32> {
    modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| u32::try_from(time.as_secs()).ok())
        .ok_or_else(|| {
            io::Error::other("A reviewed modification time is outside the SFTP v3 range")
        })
}

#[derive(Debug)]
pub struct Report {
    pub(in crate::sftp_window) note: String,
    pub(in crate::sftp_window) disconnected: bool,
    pub(in crate::sftp_window) directory: Option<Arc<Directory>>,
}

pub(in crate::sftp_window) struct Applied {
    pub(in crate::sftp_window) copied: usize,
    pub(in crate::sftp_window) created: usize,
    pub(in crate::sftp_window) replaced: usize,
    pub(in crate::sftp_window) recovery_prefix: Option<String>,
    pub(in crate::sftp_window) result: Result<()>,
}

struct StandingGuard<'a> {
    roots: &'a StandingRoots,
    authorize: &'a dyn Fn() -> io::Result<()>,
}

struct WriteChecks<'a> {
    cancelled: &'a dyn Fn() -> bool,
    disconnected: &'a dyn Fn() -> bool,
    standing: Option<StandingGuard<'a>>,
    combined: Option<&'a dyn Fn() -> Result<()>>,
    scheduled: Option<&'a dyn Fn() -> Result<()>>,
    origin: CopyOrigin,
}

enum DownloadAccess {
    Browser(download_access::Access),
    Scheduler(super::ordinary_access::Access),
}

impl DownloadAccess {
    fn apply_to(&self, file: &File) -> io::Result<()> {
        match self {
            Self::Browser(access) => access.apply_to(file),
            Self::Scheduler(access) => access.apply_to(file),
        }
    }

    fn verify_parent(&self) -> io::Result<()> {
        match self {
            Self::Browser(access) => access.verify_parent(),
            Self::Scheduler(access) => access.verify_parent(),
        }
    }
}

impl WriteChecks<'_> {
    fn running(&self) -> io::Result<()> {
        require_running(&self.cancelled)
    }

    fn before_write(&self, client: &mut Client) -> Result<()> {
        self.running()?;
        if let Some(standing) = &self.standing {
            standing.roots.verify(client, false)?;
            (standing.authorize)()?;
            self.running()?;
        }
        if let Some(authorize) = self.combined {
            authorize()?;
        }
        if let Some(authorize) = self.scheduled {
            authorize()?;
        }
        Ok(())
    }

    fn publish_download(
        &self,
        client: &mut Client,
        staged: StagedDownload,
        access: &DownloadAccess,
    ) -> Result<()> {
        // StagedDownload calls its check after syncing the staged file and
        // verifying the opened local parent, just before no-replace publication.
        let guard = RefCell::new((client, None));
        let result = staged.publish_checked(|file| {
            let (client, error) = &mut *guard.borrow_mut();
            *error = (|| -> Result<()> {
                self.before_write(client)?;
                access.apply_to(file)?;
                file.sync_all()?;
                self.before_write(client)?;
                access.verify_parent()?;
                Ok(())
            })()
            .err();
            if error.is_some() {
                Err(io::Error::other(
                    "The reviewed download could not be published",
                ))
            } else {
                Ok(())
            }
        });
        if let Some(error) = guard.into_inner().1 {
            return Err(error);
        }
        Ok(result?)
    }
}

pub(in crate::sftp_window) fn apply_reviewed(
    client: &mut Client,
    plan: &CopyPlan,
    progress: &Progress,
    cancelled: &impl Fn() -> bool,
    disconnected: &impl Fn() -> bool,
    claim: impl FnOnce() -> io::Result<()>,
) -> Applied {
    let checks = WriteChecks {
        cancelled,
        disconnected,
        standing: None,
        combined: None,
        scheduled: None,
        origin: plan.origin,
    };
    apply_checked(client, plan, progress, &checks, claim)
}

pub(in crate::sftp_window) fn apply_scheduled(
    client: &mut Client,
    plan: &CopyPlan,
    progress: &Progress,
    cancelled: &impl Fn() -> bool,
    disconnected: &impl Fn() -> bool,
    claim: impl FnOnce() -> io::Result<()>,
    authorize: &impl Fn() -> io::Result<()>,
) -> Applied {
    let authorize = || authorize().map_err(Failure::from);
    let checks = WriteChecks {
        cancelled,
        disconnected,
        standing: None,
        combined: None,
        scheduled: Some(&authorize),
        origin: plan.origin,
    };
    apply_checked(client, plan, progress, &checks, claim)
}

pub(in crate::sftp_window) fn apply_standing(
    client: &mut Client,
    plan: &CopyPlan,
    progress: &Progress,
    cancelled: &impl Fn() -> bool,
    disconnected: &impl Fn() -> bool,
    authorize: impl Fn() -> io::Result<()>,
) -> Applied {
    let Some(roots) = &plan.standing_roots else {
        return Applied {
            copied: 0,
            created: 0,
            replaced: 0,
            recovery_prefix: None,
            result: Err(io::Error::other("The standing copy folders could not be verified").into()),
        };
    };
    let checks = WriteChecks {
        cancelled,
        disconnected,
        standing: Some(StandingGuard {
            roots,
            authorize: &authorize,
        }),
        combined: None,
        scheduled: None,
        origin: plan.origin,
    };
    apply_checked(client, plan, progress, &checks, || Ok(()))
}

fn apply_checked(
    client: &mut Client,
    plan: &CopyPlan,
    progress: &Progress,
    checks: &WriteChecks<'_>,
    claim: impl FnOnce() -> io::Result<()>,
) -> Applied {
    let mut applied = Applied {
        copied: 0,
        created: 0,
        replaced: 0,
        recovery_prefix: None,
        result: Ok(()),
    };
    let mut parents = BTreeMap::new();
    let result = (|| -> Result<()> {
        checks.running()?;
        plan.verify_destination(client)?;
        plan.review.verify_current(client, checks.cancelled)?;
        checks.running()?;
        claim()?;
        if let Some(standing) = &checks.standing {
            standing.roots.verify(client, true)?;
        }
        #[cfg(windows)]
        if let Some(replacements) = &plan.replacements {
            replacements.apply(client, progress, checks, &mut applied)?;
        }
        for action in &plan.actions {
            checks.running()?;
            let cleanup = apply_action(client, action, progress, checks, &mut parents)
                .map_err(|error| error.context(format!(" · {}", action.label)))?;
            if action.creates_folder() {
                applied.created += 1;
            } else {
                applied.copied += 1;
            }
            if let Some(error) = cleanup {
                return Err(error.context(format!(" · {}", action.label)));
            }
        }
        Ok(())
    })();
    applied.result = result;
    applied
}

pub(super) fn apply(
    client: &mut Client,
    plan: &Plan,
    progress: &Progress,
    cancel: &Cancel,
    control: &Control,
) -> Report {
    let cancelled = || cancel.stopped() || control.is_closed();
    let authorize = || {
        plan.approval
            .authorize(control, &cancelled, plan.removals.is_some())
    };
    let start = (|| -> Result<()> {
        authorize()?;
        plan.approval.claim()?;
        if let Some(removals) = &plan.removals {
            removals.verify_destination(client, &cancelled)?;
        } else if plan.actions.is_empty() {
            return Err(io::Error::other("The synchronization review has no operations").into());
        }
        Ok(())
    })();
    let Applied {
        copied,
        created,
        mut result,
        ..
    } = match start {
        Ok(()) => apply_reviewed(
            client,
            plan,
            progress,
            &cancelled,
            &|| control.is_closed(),
            || authorize().map_err(|error| io::Error::other(error.message)),
        ),
        Err(error) => Applied {
            copied: 0,
            created: 0,
            replaced: 0,
            recovery_prefix: None,
            result: Err(error),
        },
    };
    let mut removed = removal::Counts::default();
    if result.is_ok()
        && let Some(removals) = &plan.removals
    {
        result = removals.apply(client, &authorize, &cancelled, &mut removed);
    }
    let mut refresh_note = String::new();
    let directory = plan.removals.as_ref().and_then(|removals| {
        if control.is_closed() || result.as_ref().is_err_and(|error| error.disconnected) {
            return None;
        }
        let root = removals.root();
        match list(client, root) {
            Ok(listing) if listing.path == *root => Some(listing),
            Ok(_) => {
                refresh_note = " · The destination folder changed before refresh".into();
                None
            }
            Err(error) => {
                if error.disconnected {
                    control.abort();
                }
                refresh_note = format!(" · Destination refresh failed ({})", error.message);
                None
            }
        }
    });
    cancel.finish();
    let (files, folders) = plan.counts();
    let folders_note = if folders == 0 {
        String::new()
    } else {
        format!(" · {created} of {folders} folders created")
    };
    let removal_note = plan
        .removals
        .as_ref()
        .map_or_else(String::new, |removals| removed.note(removals.len()));
    match result {
        Ok(()) => Report {
            note: if files == 0 && folders == 0 {
                format!("{}{refresh_note}", removal_note.trim_start_matches(" · "))
            } else if files == 0 {
                format!(
                    "Created {created} of {folders} reviewed folders{removal_note}{refresh_note}"
                )
            } else {
                format!(
                    "Copied {copied} of {files} reviewed files{folders_note}{removal_note}{refresh_note}"
                )
            },
            disconnected: control.is_closed(),
            directory,
        },
        Err(error) => {
            let status = match (plan.removals.is_some(), cancel.stopped()) {
                (true, true) => "Reviewed synchronization cancelled",
                (true, false) => "Reviewed synchronization stopped",
                (false, true) => "Reviewed copy cancelled",
                (false, false) => "Reviewed copy stopped",
            };
            let files_note = if files == 0 {
                String::new()
            } else {
                format!(" · {copied} of {files} files published")
            };
            Report {
                note: format!(
                    "{status}{files_note}{folders_note}{removal_note} · {}{refresh_note}",
                    error.message
                ),
                disconnected: error.disconnected || control.is_closed(),
                directory,
            }
        }
    }
}

fn apply_action(
    client: &mut Client,
    action: &Action,
    progress: &Progress,
    checks: &WriteChecks<'_>,
    parents: &mut BTreeMap<Option<Relative>, DirectoryStamp>,
) -> Result<Option<Failure>> {
    let target_parent = parents
        .get(&action.parent.path)
        .or(action.parent.reviewed.as_ref())
        .ok_or_else(|| io::Error::other("The reviewed destination parent was not created"))?;
    let (created, warning) = match &action.kind {
        ActionKind::Folder(source, stamp) => {
            stamp.verify(client, source, checks.combined.is_none())?;
            match mkdir(
                client,
                &action.parent.location,
                target_parent,
                &action.target,
                checks,
            )? {
                MadeFolder::Verified(child) => (Some(child), None),
                MadeFolder::Unverified(error) => (None, Some(error)),
            }
        }
        ActionKind::File {
            source,
            stamp,
            parent,
            digest,
        } => {
            let transfer = FileTransfer {
                source,
                stamp,
                source_parent: parent,
                target: &action.target,
                target_folder: &action.parent.location,
                target_parent,
                digest: *digest,
            };
            (None, copy(client, transfer, progress, checks)?)
        }
    };
    if warning.is_some() {
        return Ok(warning);
    }
    if matches!(&action.parent.location, Location::Remote(_)) {
        let current = match target_parent.verify(client, &action.parent.location, false) {
            Ok(current) => current,
            Err(error) => return Ok(Some(error)),
        };
        parents.insert(action.parent.path.clone(), current);
    }
    if let Some(created) = created {
        parents.insert(Some(action.path.clone()), created);
    }
    Ok(None)
}

enum MadeFolder {
    Verified(DirectoryStamp),
    Unverified(Failure),
}

fn mkdir(
    client: &mut Client,
    folder: &Location,
    stamp: &DirectoryStamp,
    target: &Location,
    checks: &WriteChecks<'_>,
) -> Result<MadeFolder> {
    use super::resume::persist::Identity;

    let check = match (folder, stamp, target) {
        (Location::Local(folder), DirectoryStamp::Local(expected), Location::Local(target)) => {
            let parent = local_tree::Directory::open_path(folder)?;
            let name = target
                .file_name()
                .filter(|_| target.parent() == Some(folder.as_path()))
                .ok_or_else(|| io::Error::other("The reviewed local folder path is invalid"))?;
            if Identity::directory(&parent)? != *expected {
                return Err(
                    io::Error::other("The reviewed local destination parent changed").into(),
                );
            }
            checks.before_write(client)?;
            let child = parent.create_transfer_child(name)?;
            (|| -> Result<DirectoryStamp> {
                let parent_now = local_tree::Directory::open_path(folder)?;
                let local_tree::Entry::Directory(child_now) = local_tree::entry(&parent_now, name)?
                else {
                    return Err(io::Error::other("The created local folder changed").into());
                };
                if !parent.same_as(&parent_now)? || !child.same_as(&child_now)? {
                    return Err(io::Error::other("The created local folder changed").into());
                }
                Ok(DirectoryStamp::Local(Identity::directory(&child)?))
            })()
        }
        (Location::Remote(_), DirectoryStamp::Remote(parent), Location::Remote(target)) => {
            let uid = require_private_parent(*parent)?;
            require_absent(client, target)?;
            stamp.verify(client, folder, true)?;
            checks.before_write(client)?;
            client.mkdir_private(target).map_err(|error| {
                let error = Failure::from(error);
                if error.disconnected {
                    error.context(" · Server folder creation could not be confirmed")
                } else {
                    error
                }
            })?;
            RemoteStage::owned_directory(client, target, uid).map(DirectoryStamp::Remote)
        }
        _ => {
            return Err(
                io::Error::other("The reviewed folder creation direction is invalid").into(),
            );
        }
    };
    Ok(match check {
        Ok(stamp) => MadeFolder::Verified(stamp),
        Err(error) => MadeFolder::Unverified(
            error.context(" · The folder was created but could not be verified"),
        ),
    })
}

struct FileTransfer<'a> {
    source: &'a Location,
    stamp: &'a SnapshotEntry,
    source_parent: &'a (Location, DirectoryStamp),
    target: &'a Location,
    target_folder: &'a Location,
    target_parent: &'a DirectoryStamp,
    digest: Option<[u8; 32]>,
}

fn copy(
    client: &mut Client,
    transfer: FileTransfer<'_>,
    progress: &Progress,
    checks: &WriteChecks<'_>,
) -> Result<Option<Failure>> {
    let (source_folder, source_parent) = transfer.source_parent;
    let FileTransfer {
        source,
        target,
        stamp,
        target_folder,
        target_parent,
        digest,
        ..
    } = transfer;
    let source_parent = &source_parent.verify(client, source_folder, checks.combined.is_none())?;
    checks.running()?;
    match (source, target, stamp, source_parent, target_parent) {
        (
            Location::Remote(source),
            Location::Local(target),
            SnapshotEntry::Remote(stamp),
            _,
            DirectoryStamp::Local(parent),
        ) => {
            target_parent.verify(client, target_folder, true)?;
            checks.before_write(client)?;
            let Location::Local(folder) = target_folder else {
                return Err(io::Error::other("The local destination parent is invalid").into());
            };
            let access =
                match checks.origin {
                    CopyOrigin::Browser => DownloadAccess::Browser(
                        download_access::Access::capture(folder, parent, stamp.permissions)?,
                    ),
                    CopyOrigin::Scheduler => DownloadAccess::Scheduler(
                        super::ordinary_access::Access::capture(folder, parent)?,
                    ),
                };
            let staged = StagedDownload::stage_new_download(target, parent)?;
            staged.verify_reviewed_parent(parent)?;
            let file = staged.writer()?;
            let mut observed = digest
                .map(|_| HashAlgorithm::Sha256.new_hasher().map_err(io::Error::other))
                .transpose()?;
            let output = ObserveOutput(Counted(progress, file), |bytes: &[u8]| {
                if let Some(hash) = observed.as_mut() {
                    hash.update(bytes);
                }
            });
            client.download_regular(source, *stamp, CheckedOutput(output, &checks.cancelled))?;
            if observed.map(|hash| hash_bytes(hash.finalize())) != digest {
                return Err(changed_bytes().into());
            }
            if matches!(checks.origin, CopyOrigin::Browser)
                && let Some(modified) = stamp.modified
            {
                checks.before_write(client)?;
                file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(modified)))?;
            }
            source_parent.verify(client, source_folder, true)?;
            staged.verify_partial(&FileStamp::read(file)?)?;
            checks.publish_download(client, staged, &access)?;
            Ok(None)
        }
        (
            Location::Local(source),
            Location::Remote(path),
            SnapshotEntry::LocalFile { stamp, identity },
            DirectoryStamp::Local(parent),
            _,
        ) => {
            let local = LocalSource::open(source)?;
            let verify = || local.verify_reviewed(stamp, identity, parent);
            verify()?;
            let target = RemoteTarget {
                folder: target_folder,
                path,
                stamp: target_parent,
                modified: wire_modified(stamp.modified.ok_or_else(|| {
                    io::Error::other("The reviewed source has no modification time")
                })?)?,
                digest,
            };
            upload(client, target, &local, progress, checks, verify)
        }
        _ => Err(io::Error::other("The reviewed transfer direction is invalid").into()),
    }
}

struct RemoteTarget<'a> {
    folder: &'a Location,
    path: &'a [u8],
    stamp: &'a DirectoryStamp,
    modified: u32,
    digest: Option<[u8; 32]>,
}

fn upload(
    client: &mut Client,
    target: RemoteTarget<'_>,
    source: &LocalSource,
    progress: &Progress,
    checks: &WriteChecks<'_>,
    verify_source: impl Fn() -> io::Result<()>,
) -> Result<Option<Failure>> {
    let (Location::Remote(folder), DirectoryStamp::Remote(attributes)) =
        (target.folder, target.stamp)
    else {
        return Err(io::Error::other("The reviewed remote destination has no folder").into());
    };
    let uid = require_private_parent(*attributes)?;
    require_absent(client, target.path)?;
    target.stamp.verify(client, target.folder, true)?;
    checks.before_write(client)?;
    let stage = RemoteStage::claim(client, folder, uid)?;
    let mut owner = None;
    let mut publishing = false;
    let result = (|| -> Result<Option<Failure>> {
        checks.before_write(client)?;
        client.upload_private_with_modified(
            &stage.payload,
            source.size(),
            target.modified,
            Counted(progress, source.file()),
            |seen, _| {
                if seen.uid != stage.uid {
                    return Err(io::Error::other("The private upload payload owner changed"));
                }
                owner = Some(seen);
                verify_source()
            },
            checks.cancelled,
        )?;
        let owner = owner
            .ok_or_else(|| io::Error::other("The private upload stage could not be verified"))?;
        let mut observed = target
            .digest
            .map(|_| HashAlgorithm::Sha256.new_hasher().map_err(io::Error::other))
            .transpose()?;
        let input = ObserveInput(source.file(), |bytes: &[u8]| {
            if let Some(hash) = observed.as_mut() {
                hash.update(bytes);
            }
        });
        if client.verify_private_upload(
            &stage.payload,
            owner,
            source.size(),
            input,
            checks.cancelled,
        )? != source.size()
        {
            return Err(io::Error::other("The staged upload is incomplete").into());
        }
        if observed.map(|hash| hash_bytes(hash.finalize())) != target.digest {
            return Err(changed_bytes().into());
        }
        verify_source()?;
        target.stamp.verify(client, target.folder, false)?;
        stage.verify(client)?;
        require_absent(client, target.path)?;
        let staged = client.lstat(&stage.payload)?;
        if staged.modified != Some(target.modified)
            || staged.kind() != FileKind::File
            || staged.size != Some(source.size())
            || staged.uid != Some(owner.uid)
            || staged.gid != Some(owner.gid)
            || staged.permissions.is_none_or(|mode| mode & 0o7777 != 0o600)
        {
            return Err(io::Error::other("The staged file changed before publication").into());
        }
        checks.before_write(client)?;
        publishing = true;
        client.hardlink(&stage.payload, target.path)?;
        let cleanup = stage.cleanup(client, &target).err().map(|error| {
            error.context(format!(
                " · The destination was created. Check the remote staging path {}",
                stage.label()
            ))
        });
        Ok(cleanup)
    })();
    result.map_err(|error| {
        if !error.disconnected
            && !(checks.disconnected)()
            && stage
                .verify_payload(client, owner, source, &checks.disconnected, &verify_source)
                .is_ok()
            && stage.cleanup(client, &target).is_ok()
        {
            return error;
        }
        let error = if publishing && error.disconnected {
            error.context(" · Server publication could not be confirmed")
        } else {
            error
        };
        error.context(format!(
            " · Check the remote staging path {}",
            stage.label()
        ))
    })
}

fn require_private_parent(parent: Attributes) -> Result<u32> {
    if parent.kind() == FileKind::Directory
        && parent.permissions.is_some_and(|mode| mode & 0o022 == 0)
        && parent.gid.is_some()
        && let Some(uid) = parent.uid
    {
        Ok(uid)
    } else {
        Err(io::Error::other("Reviewed uploads require a server folder with known ownership and no group or public write access").into())
    }
}

pub(super) struct RemoteStage {
    directory: Vec<u8>,
    pub(super) payload: Vec<u8>,
    pub(super) uid: u32,
    pub(super) gid: u32,
}

impl RemoteStage {
    pub(super) fn claim(client: &mut Client, parent: &[u8], uid: u32) -> Result<Self> {
        Self::claim_named(client, parent, files::nonce()?.as_bytes(), uid)
    }

    fn claim_named(client: &mut Client, parent: &[u8], name: &[u8], uid: u32) -> Result<Self> {
        files::validate_transfer_name(name)?;
        let directory = files::remote_join(parent, name);
        client.mkdir_private(&directory).map_err(|error| {
            let failure = Failure::from(error);
            if failure.disconnected {
                failure.context(format!(
                    " · Check the remote staging path {}",
                    directory.escape_ascii()
                ))
            } else {
                failure
            }
        })?;
        let attributes = Self::owned_directory(client, &directory, uid).map_err(|error| {
            error.context(format!(
                " · Check the remote staging path {}",
                directory.escape_ascii()
            ))
        })?;
        let gid = attributes.gid.expect("verified directory group");
        let payload = files::remote_join(&directory, b"payload");
        Ok(Self {
            directory,
            payload,
            uid,
            gid,
        })
    }

    fn reopen(client: &mut Client, directory: Vec<u8>, uid: u32, gid: u32) -> Result<Self> {
        let payload = files::remote_join(&directory, b"payload");
        let stage = Self {
            directory,
            payload,
            uid,
            gid,
        };
        stage.verify(client)?;
        Ok(stage)
    }

    pub(super) fn label(&self) -> String {
        self.directory.escape_ascii().to_string()
    }

    fn owned_directory(client: &mut Client, directory: &[u8], uid: u32) -> Result<Attributes> {
        let attributes = client.lstat(directory)?;
        if attributes.kind() == FileKind::Directory
            && attributes.uid == Some(uid)
            && attributes
                .permissions
                .is_some_and(|mode| mode & 0o7777 == 0o700)
            && attributes.gid.is_some()
            && client.canonicalize(directory)? == directory
        {
            Ok(attributes)
        } else {
            Err(io::Error::other(
                "The new private server folder changed or has unexpected ownership",
            )
            .into())
        }
    }

    pub(super) fn verify(&self, client: &mut Client) -> Result<()> {
        if Self::owned_directory(client, &self.directory, self.uid)?.gid == Some(self.gid) {
            Ok(())
        } else {
            Err(io::Error::other("The private upload staging folder group changed").into())
        }
    }

    fn verify_payload(
        &self,
        client: &mut Client,
        owner: Option<UploadOwner>,
        source: &LocalSource,
        disconnected: &impl Fn() -> bool,
        verify_source: &impl Fn() -> io::Result<()>,
    ) -> Result<()> {
        self.verify(client)?;
        if let Some(owner) = owner {
            verify_source()?;
            client.verify_private_upload(
                &self.payload,
                owner,
                source.size(),
                source.file(),
                disconnected,
            )?;
            verify_source()?;
        } else if remote_existing(client, &self.payload)?.is_some() {
            return Err(
                io::Error::other("The private upload payload could not be verified").into(),
            );
        }
        Ok(())
    }

    fn cleanup(&self, client: &mut Client, target: &RemoteTarget<'_>) -> Result<()> {
        target.stamp.verify(client, target.folder, false)?;
        self.remove(client)
    }

    pub(super) fn remove(&self, client: &mut Client) -> Result<()> {
        self.verify(client)?;
        match client.remove_file(&self.payload) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        self.verify(client)?;
        client.remove_dir(&self.directory)?;
        Ok(())
    }
}

fn require_absent(client: &mut Client, target: &[u8]) -> Result<()> {
    if remote_existing(client, target)?.is_some() {
        Err(io::Error::other("The reviewed destination is now occupied").into())
    } else {
        Ok(())
    }
}

fn require_running(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "Reviewed copy cancelled",
        ))
    } else {
        Ok(())
    }
}

impl<T: Seek> Seek for Counted<'_, T> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.1.seek(position)
    }
}

struct CheckedOutput<'a, W, C>(W, &'a C);

struct ObserveOutput<W, F>(W, F);

impl<W: Write, F: FnMut(&[u8])> Write for ObserveOutput<W, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.0.write(bytes)?;
        (self.1)(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

struct ObserveInput<R, F>(R, F);

impl<R: Read, F: FnMut(&[u8])> Read for ObserveInput<R, F> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.0.read(bytes)?;
        (self.1)(&bytes[..count]);
        Ok(count)
    }
}

impl<R: Seek, F> Seek for ObserveInput<R, F> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

impl<W: Write, C: Fn() -> bool> Write for CheckedOutput<'_, W, C> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        require_running(self.1)?;
        self.0.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        require_running(self.1)?;
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_upload_modification_time_checks_the_v3_range_and_precision() {
        let last = UNIX_EPOCH + Duration::from_secs(u64::from(u32::MAX));
        assert_eq!(
            wire_modified(last + Duration::from_millis(999)).unwrap(),
            u32::MAX
        );
        assert!(wire_modified(last + Duration::from_secs(1)).is_err());
        assert_eq!(wire_modified(UNIX_EPOCH).unwrap(), 0);
        assert!(wire_modified(UNIX_EPOCH - Duration::from_millis(1)).is_err());
    }

    #[test]
    fn reviewed_copy_discloses_raw_roots_and_rejects_overlong_escaped_roots() {
        let raw = Location::Remote(b"/raw/\xff/\xc3\xa9\\x41".to_vec());
        assert_eq!(
            review_root(&raw).unwrap(),
            r"Server · /raw/\xff/\xc3\xa9\\x41"
        );
        let mut boundary = vec![b'a'; MAX_LABEL_BYTES - 8];
        boundary[0] = b'/';
        boundary.extend_from_slice("é".as_bytes());
        assert!(review_root(&Location::Remote(boundary.clone())).is_ok());
        boundary.push(b'x');
        assert!(review_root(&Location::Remote(boundary)).is_err());
    }

    #[test]
    fn reviewed_local_mkdir_rejects_occupied_names_and_a_replaced_parent() {
        use std::io::Cursor;

        use super::super::resume::persist::Identity;

        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let parent = root.join("parent");
        let moved = root.join("moved");
        fs::create_dir(&parent).unwrap();
        let stamp = DirectoryStamp::Local(
            Identity::directory(&local_tree::Directory::open_path(&parent).unwrap()).unwrap(),
        );
        let folder = Location::Local(parent.clone());
        let target = Location::Local(parent.join("child"));
        let mut client: Client = Sftp::new(
            Box::new(Cursor::new([0u8, 0, 0, 5, 2, 0, 0, 0, 3])) as _,
            Box::new(io::sink()) as _,
        )
        .unwrap();
        let checks = WriteChecks {
            cancelled: &|| false,
            disconnected: &|| false,
            standing: None,
            combined: None,
            scheduled: None,
            origin: CopyOrigin::Browser,
        };
        assert!(matches!(
            mkdir(&mut client, &folder, &stamp, &target, &checks).unwrap(),
            MadeFolder::Verified(_)
        ));
        fs::write(parent.join("child/kept"), b"existing").unwrap();
        assert!(mkdir(&mut client, &folder, &stamp, &target, &checks).is_err());
        assert_eq!(fs::read(parent.join("child/kept")).unwrap(), b"existing");
        fs::rename(&parent, &moved).unwrap();
        fs::create_dir(&parent).unwrap();
        let new = Location::Local(parent.join("new"));
        assert!(mkdir(&mut client, &folder, &stamp, &new, &checks).is_err());
        assert!(!parent.join("new").exists());
        assert!(!moved.join("new").exists());
    }
}
