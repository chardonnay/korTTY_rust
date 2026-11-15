//! Device-local consent and publication state for recurring rsync transfers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::TryLockError;
use std::io::{self, Read as _};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use kortty_jobs::{DependencyMode, FiringOccurrence, Job, JobId, JobSchedule, RunId, TimeZone};
use kortty_server_archive::rsync_tree::ExistingTree;
use kortty_ssh::Endpoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::{RemoteAction, SshCommand, sync::valid_nonce};
use super::{Approval, ConnectionRef, LocalExistingSnapshot, MixedReceiver, RemotePath};
use crate::private_files::{Directory, nonce};
use crate::ssh_dialog::LoginTarget;

const FILE: &str = "job-rsync-standing-reviews.json";
const LOCK: &str = ".job-rsync-standing-reviews.lock";
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const DAY: u64 = 24 * 60 * 60;
const MAX_DAYS: u16 = 30;
const MAX_FIRINGS: u16 = 128;
const MAX_ENTRIES: u32 = super::MAX_TREE_ENTRIES as u32;
const OVERLAPPING_REMOTE: &str = "Choose separate, non-overlapping remote destinations for recurring rsync targets on the same SSH route.";

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) days: u16,
    pub(crate) firings: u16,
    pub(crate) budget: Budget,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            days: 7,
            firings: 12,
            budget: Budget {
                bytes: 32 * 1024 * 1024,
                entries: 64,
                deletes: 0,
            },
        }
    }
}

impl Settings {
    pub(in crate::scheduler_window) fn validate(self) -> io::Result<()> {
        if !(1..=MAX_DAYS).contains(&self.days)
            || !(1..=MAX_FIRINGS).contains(&self.firings)
            || !(1..=super::MAX_FILE_BYTES).contains(&self.budget.bytes)
            || !(1..=MAX_ENTRIES).contains(&self.budget.entries)
            || self.budget.deletes > MAX_ENTRIES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Choose 1–30 days, 1–128 firings, up to 4,096 entries or deletions and up to 256 MiB per firing.",
            ));
        }
        Ok(())
    }

    pub(super) fn expiry(self, now: SystemTime) -> io::Result<SystemTime> {
        self.validate()?;
        now.checked_add(Duration::from_secs(DAY * u64::from(self.days)))
            .ok_or_else(needs_review)
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) settings: Settings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Budget {
    pub(crate) bytes: u64,
    pub(crate) entries: u32,
    pub(crate) deletes: u32,
}

impl Budget {
    fn plus(self, cost: Self, limits: Self) -> io::Result<Self> {
        let total = self
            .bytes
            .checked_add(cost.bytes)
            .zip(self.entries.checked_add(cost.entries))
            .zip(self.deletes.checked_add(cost.deletes))
            .map(|((bytes, entries), deletes)| Self {
                bytes,
                entries,
                deletes,
            });
        total
            .filter(|sum| {
                sum.bytes <= limits.bytes
                    && sum.entries <= limits.entries
                    && sum.deletes <= limits.deletes
            })
            .ok_or_else(|| {
                io::Error::other("The combined rsync routes exceed the limits for this firing.")
            })
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Receiver {
    Remote {
        destination: RemotePath,
        snapshot: ExistingTree,
    },
    Local {
        destination: PathBuf,
        snapshot: LocalExistingSnapshot,
    },
    LocalFile {
        destination: PathBuf,
        snapshot: crate::sftp_window::LocalFileSnapshot,
    },
}

impl Receiver {
    pub(super) fn from_review(review: &Approval) -> io::Result<Self> {
        let receiver = match review.mixed.as_deref().map(|review| &review.receiver) {
            Some(MixedReceiver::Server {
                destination,
                snapshot: Some(snapshot),
            }) => Self::Remote {
                destination: destination.clone(),
                snapshot: *snapshot,
            },
            Some(MixedReceiver::Local {
                destination,
                snapshot: Some(snapshot),
            }) => Self::Local {
                destination: destination.clone(),
                snapshot: *snapshot,
            },
            Some(_) => return Err(existing_only()),
            None => {
                if let Some(remote) = &review.existing {
                    Self::Remote {
                        destination: remote.destination.clone(),
                        snapshot: remote.snapshot,
                    }
                } else if let Some(local) = &review.existing_download {
                    Self::Local {
                        destination: local.destination.clone(),
                        snapshot: local.snapshot,
                    }
                } else if let Some(local) = &review.existing_file_download {
                    Self::LocalFile {
                        destination: local.destination.clone(),
                        snapshot: local.snapshot,
                    }
                } else {
                    return Err(existing_only());
                }
            }
        };
        receiver
            .valid()
            .then_some(receiver)
            .ok_or_else(needs_review)
    }

    fn valid(&self) -> bool {
        match self {
            Self::Remote {
                destination,
                snapshot,
            } => super::valid_remote(destination.as_str()) && snapshot.inventory.valid(),
            Self::Local {
                destination,
                snapshot,
            } => super::super::remote::validate_local_path(destination).is_ok() && snapshot.valid(),
            Self::LocalFile {
                destination,
                snapshot,
            } => super::super::remote::validate_local_path(destination).is_ok() && snapshot.valid(),
        }
    }

    fn same_destination(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Remote { destination: a, .. }, Self::Remote { destination: b, .. }) => a == b,
            (Self::Local { destination: a, .. }, Self::Local { destination: b, .. }) => a == b,
            (Self::LocalFile { destination: a, .. }, Self::LocalFile { destination: b, .. }) => {
                a == b
            }
            _ => false,
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::scheduler_window) struct Triggers {
    pub(in crate::scheduler_window) schedule: Option<JobSchedule>,
    pub(in crate::scheduler_window) mode: DependencyMode,
    pub(in crate::scheduler_window) predecessors: Vec<JobId>,
    pub(in crate::scheduler_window) max_age_minutes: Option<std::num::NonZeroU32>,
}

impl Triggers {
    fn from_job(job: &Job<SshCommand>) -> Self {
        Self {
            schedule: job.schedule().cloned(),
            mode: job.mode(),
            predecessors: job.dependencies().to_vec(),
            max_age_minutes: job.max_age_minutes(),
        }
    }

    pub(super) fn calendar(&self) -> bool {
        self.schedule.as_ref().is_some_and(JobSchedule::enabled)
    }

    pub(super) fn chain(&self) -> bool {
        self.mode == DependencyMode::Chain && !self.predecessors.is_empty()
    }

    pub(in crate::scheduler_window) fn validate(&self) -> Result<(), &'static str> {
        if self.calendar() || self.chain() {
            Ok(())
        } else {
            Err("Recurring rsync requires an enabled calendar or Chain mode with predecessors.")
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(in crate::scheduler_window) struct Intent {
    pub(in crate::scheduler_window) settings: Settings,
    pub(in crate::scheduler_window) triggers: Triggers,
    pub(in crate::scheduler_window) zone: String,
}

fn scope(
    job: &Job<SshCommand>,
    zone: &TimeZone,
) -> io::Result<([u8; 32], Option<String>, Settings)> {
    let mut action = job.action().clone();
    let transfer = match action.remote.as_mut() {
        Some(RemoteAction::RsyncUpload(transfer) | RemoteAction::RsyncDownload(transfer)) => {
            transfer
        }
        _ => return Err(needs_review()),
    };
    let config = transfer.standing.as_mut().ok_or_else(needs_review)?;
    let approval = config.approval.take();
    let settings = config.settings;
    settings.validate()?;
    let triggers = Triggers::from_job(job);
    triggers.validate().map_err(io::Error::other)?;
    let zone = zone.iana_name().ok_or_else(|| {
        io::Error::other("Choose a named time zone before approving recurring rsync.")
    })?;
    let bytes = serde_json::to_vec(&(
        job.id(),
        triggers.schedule,
        triggers.mode,
        triggers.predecessors,
        triggers.max_age_minutes,
        zone,
        action,
    ))?;
    let mut hash = Sha256::new_with_prefix(b"kortty-scheduled-rsync-standing-v1\0");
    hash.update(bytes);
    Ok((hash.finalize().into(), approval, settings))
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    pub(crate) approval: String,
    scope: [u8; 32],
    issued_at: SystemTime,
    expires_at: SystemTime,
    settings: Settings,
    routes: Vec<Route>,
}

impl Grant {
    pub(crate) fn new(
        job: &Job<SshCommand>,
        zone: &TimeZone,
        issued_at: SystemTime,
    ) -> io::Result<Self> {
        let (scope, _, settings) = scope(job, zone)?;
        let (transfer, upload) = job
            .action()
            .remote
            .as_ref()
            .and_then(RemoteAction::rsync_transfer)
            .ok_or_else(needs_review)?;
        job.action()
            .validate_target_selection()
            .map_err(io::Error::other)?;
        transfer.require_group_review().map_err(io::Error::other)?;
        let mut remote_routes: Vec<(&ConnectionRef, Vec<String>, RemotePath)> = Vec::new();
        let routes = job
            .action()
            .targets()
            .enumerate()
            .map(|(index, reference)| {
                let transfer = transfer
                    .for_target(index, upload)
                    .map_err(io::Error::other)?;
                let review = transfer.approval.as_ref().ok_or_else(needs_review)?;
                if review.reference != *reference {
                    return Err(needs_review());
                }
                let receiver = Receiver::from_review(review)?;
                if let Receiver::Remote { destination, .. } = &receiver {
                    if remote_routes.iter().any(|(previous, pins, path)| {
                        pins == &review.pins
                            && same_login(&previous.target, &reference.target)
                            && match (&previous.jump, &reference.jump) {
                                (None, None) => true,
                                (Some(a), Some(b)) => same_login(a, b),
                                _ => false,
                            }
                            && (path.contains(destination) || destination.contains(path))
                    }) {
                        return Err(io::Error::other(OVERLAPPING_REMOTE));
                    }
                    remote_routes.push((reference, review.pins.clone(), destination.clone()));
                }
                Ok(Route {
                    reference: reference.clone(),
                    receiver,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let grant = Self {
            approval: nonce()?,
            scope,
            issued_at,
            expires_at: settings.expiry(issued_at)?,
            settings,
            routes,
        };
        grant.valid().then_some(grant).ok_or_else(needs_review)
    }

    pub(in crate::scheduler_window) fn matches(
        &self,
        job: &Job<SshCommand>,
        zone: &TimeZone,
        now: SystemTime,
    ) -> bool {
        self.active(now) && scope(job, zone).is_ok_and(|(scope, _, _)| self.scope == scope)
    }

    fn active(&self, now: SystemTime) -> bool {
        self.issued_at <= now && now < self.expires_at
    }

    fn valid(&self) -> bool {
        valid_nonce(&self.approval)
            && self
                .settings
                .expiry(self.issued_at)
                .is_ok_and(|expiry| expiry == self.expires_at)
            && (1..=super::super::connection::MAX_TARGETS).contains(&self.routes.len())
            && self
                .routes
                .iter()
                .map(|route| route.reference.id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.routes.len()
            && self
                .routes
                .iter()
                .all(|route| route.reference.validate().is_ok() && route.receiver.valid())
    }
}

fn same_login(a: &LoginTarget, b: &LoginTarget) -> bool {
    a.username == b.username
        && a.port == b.port
        && Endpoint::new(&a.host, a.port).is_ok_and(|left| {
            Endpoint::new(&b.host, b.port).is_ok_and(|right| left.wire_host() == right.wire_host())
        })
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Route {
    reference: ConnectionRef,
    receiver: Receiver,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Ready,
    Reserved,
    Armed,
    Finished,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Current {
    nonce: String,
    budget: Budget,
    phases: Vec<Phase>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stored {
    grant: Grant,
    firings: u16,
    latest_calendar: Option<SystemTime>,
    latest_predecessors: BTreeMap<JobId, (SystemTime, RunId)>,
    // Keep a bound after a predecessor leaves the current round and its ID is discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retired_predecessor_at: Option<SystemTime>,
    current: Option<Current>,
}

impl Stored {
    fn armed(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|current| current.phases.contains(&Phase::Armed))
    }

    fn valid(&self) -> bool {
        self.grant.valid()
            && self.firings <= self.grant.settings.firings
            && (self.firings == 0) == self.current.is_none()
            && self.latest_predecessors.len() <= kortty_jobs::MAX_DEPENDENCIES
            && self.current.as_ref().is_none_or(|current| {
                valid_nonce(&current.nonce)
                    && current.phases.len() == self.grant.routes.len()
                    && Budget::default()
                        .plus(current.budget, self.grant.settings.budget)
                        .is_ok()
            })
    }
}

#[derive(Clone)]
pub(crate) struct Store(PathBuf);

type Index = BTreeMap<JobId, Stored>;

impl Store {
    pub(crate) fn new(profile: PathBuf) -> Self {
        Self(profile)
    }

    pub(crate) fn swap(
        &self,
        job: &JobId,
        grant: Grant,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<Option<Stored>> {
        if !grant.valid() || !grant.active(SystemTime::now()) {
            return Err(needs_review());
        }
        self.change(|index| {
            index.retain(|id, stored| id == job || keep(id) || stored.armed());
            let prior = index.get(job);
            let latest_calendar = prior.and_then(|prior| prior.latest_calendar);
            let latest_predecessors = prior
                .map(|prior| prior.latest_predecessors.clone())
                .unwrap_or_default();
            let retired_predecessor_at = prior.and_then(|prior| prior.retired_predecessor_at);
            Ok(index.insert(
                job.clone(),
                Stored {
                    grant,
                    firings: 0,
                    latest_calendar,
                    latest_predecessors,
                    retired_predecessor_at,
                    current: None,
                },
            ))
        })
    }

    pub(in crate::scheduler_window) fn restore(
        &self,
        job: &JobId,
        approval: &str,
        prior: Option<Stored>,
    ) -> io::Result<()> {
        self.change(|index| {
            if !index
                .get(job)
                .is_some_and(|stored| stored.grant.approval == approval && stored.firings == 0)
            {
                return Err(needs_review());
            }
            if let Some(prior) = prior {
                index.insert(job.clone(), prior);
            } else {
                index.remove(job);
            }
            Ok(())
        })
    }

    pub(in crate::scheduler_window) fn saved(
        &self,
        job: &Job<SshCommand>,
        zone: &TimeZone,
        now: SystemTime,
    ) -> io::Result<()> {
        let (scope, approval, _) = scope(job, zone)?;
        let approval = approval.ok_or_else(needs_review)?;
        read_index(&Directory::open(&self.0, false)?)?
            .get(job.id())
            .filter(|stored| {
                stored.grant.scope == scope
                    && stored.grant.approval == approval
                    && stored.grant.active(now)
                    && stored.firings < stored.grant.settings.firings
            })
            .ok_or_else(needs_review)
            .and_then(|stored| {
                if stored.armed() {
                    Err(uncertain())
                } else {
                    Ok(())
                }
            })
    }

    pub(crate) fn claim(
        &self,
        job: &Job<SshCommand>,
        zone: &TimeZone,
        occurrence: FiringOccurrence,
        now: SystemTime,
    ) -> io::Result<Lease> {
        if !job.enabled() {
            return Err(needs_review());
        }
        let (scope, approval, _) = scope(job, zone)?;
        let approval = approval.ok_or_else(needs_review)?;
        let firing = nonce()?;
        let triggers = Triggers::from_job(job);
        self.change(|index| {
            let stored = index
                .get_mut(job.id())
                .filter(|stored| {
                    stored.grant.scope == scope
                        && stored.grant.approval == approval
                        && stored.grant.active(now)
                        && stored.grant.active(SystemTime::now())
                        && stored.firings < stored.grant.settings.firings
                })
                .ok_or_else(needs_review)?;
            if stored.armed() {
                return Err(uncertain());
            }
            match occurrence {
                FiringOccurrence::Calendar(instant)
                    if triggers.calendar()
                        && stored.grant.issued_at <= instant
                        && instant <= now
                        && stored.latest_calendar.is_none_or(|latest| instant > latest) =>
                {
                    stored.latest_calendar = Some(instant);
                }
                FiringOccurrence::Predecessors(round)
                    if triggers.chain()
                        && round.len() == triggers.predecessors.len()
                        && round.iter().zip(&triggers.predecessors).all(
                            |((id, run, finished), expected)| {
                                id == expected
                                    && run.value() > 0
                                    && *finished <= now
                                    && match stored.latest_predecessors.get(id) {
                                        Some(latest) => (*finished, *run) > *latest,
                                        None => stored
                                            .retired_predecessor_at
                                            .is_none_or(|retired| *finished > retired),
                                    }
                            },
                        ) =>
                {
                    let latest = round
                        .into_iter()
                        .map(|(id, run, end)| (id, (end, run)))
                        .collect::<BTreeMap<_, _>>();
                    stored.retired_predecessor_at = stored.retired_predecessor_at.max(
                        stored
                            .latest_predecessors
                            .iter()
                            .filter(|(id, _)| !latest.contains_key(*id))
                            .map(|(_, (finished, _))| *finished)
                            .max(),
                    );
                    stored.latest_predecessors = latest;
                }
                _ => return Err(needs_review()),
            }
            stored.firings += 1;
            stored.current = Some(Current {
                nonce: firing.clone(),
                budget: Budget::default(),
                phases: vec![Phase::Ready; stored.grant.routes.len()],
            });
            Ok(())
        })?;
        Ok(Lease {
            store: self.clone(),
            job: job.id().clone(),
            approval,
            firing,
        })
    }

    fn change<T>(&self, change: impl FnOnce(&mut Index) -> io::Result<T>) -> io::Result<T> {
        let directory = Directory::open(&self.0, false)?;
        let lock = directory.open_lock(LOCK)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        let mut index = read_index(&directory)?;
        let result = change(&mut index)?;
        let bytes = serde_json::to_vec(&index)?;
        if !valid_index(&index) || bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(needs_review());
        }
        directory.replace_file(FILE, &bytes)?;
        directory.sync()?;
        Ok(result)
    }
}

#[derive(Clone)]
pub(crate) struct Lease {
    store: Store,
    job: JobId,
    approval: String,
    firing: String,
}

impl Lease {
    fn stored<'a>(&self, index: &'a Index, active: bool) -> io::Result<&'a Stored> {
        index
            .get(&self.job)
            .filter(|stored| self.matches(stored, active))
            .ok_or_else(needs_review)
    }

    fn matches(&self, stored: &Stored, active: bool) -> bool {
        stored.grant.approval == self.approval
            && (!active || stored.grant.active(SystemTime::now()))
            && stored
                .current
                .as_ref()
                .is_some_and(|current| current.nonce == self.firing)
    }

    pub(crate) fn require_current(&self) -> io::Result<()> {
        let index = read_index(&Directory::open(&self.store.0, false)?)?;
        self.stored(&index, true).map(|_| ())
    }

    pub(crate) fn remaining(&self) -> io::Result<Budget> {
        let index = read_index(&Directory::open(&self.store.0, false)?)?;
        let stored = self.stored(&index, true)?;
        let used = stored.current.as_ref().ok_or_else(needs_review)?.budget;
        let limits = stored.grant.settings.budget;
        Ok(Budget {
            bytes: limits.bytes - used.bytes,
            entries: limits.entries - used.entries,
            deletes: limits.deletes - used.deletes,
        })
    }

    pub(crate) fn expected_receiver(&self, reference: &ConnectionRef) -> io::Result<Receiver> {
        let index = read_index(&Directory::open(&self.store.0, false)?)?;
        self.stored(&index, true)?
            .grant
            .routes
            .iter()
            .find(|route| &route.reference == reference)
            .map(|route| route.receiver.clone())
            .ok_or_else(needs_review)
    }

    pub(crate) fn reserve(&self, reference: &ConnectionRef, budget: Budget) -> io::Result<()> {
        if budget.deletes != 0 {
            return Err(needs_review());
        }
        self.change_route(reference, true, Phase::Ready, |current, route, limits| {
            current.budget = current.budget.plus(budget, limits)?;
            current.phases[route] = Phase::Reserved;
            Ok(None)
        })
    }

    pub(crate) fn arm(&self, reference: &ConnectionRef, removed: u32) -> io::Result<()> {
        self.change_route(
            reference,
            true,
            Phase::Reserved,
            |current, route, limits| {
                current.budget = current.budget.plus(
                    Budget {
                        deletes: removed,
                        ..Budget::default()
                    },
                    limits,
                )?;
                current.phases[route] = Phase::Armed;
                Ok(None)
            },
        )
    }

    /// Call only with the native engine's confirmed, held successor.
    pub(crate) fn confirm(&self, reference: &ConnectionRef, successor: Receiver) -> io::Result<()> {
        if !successor.valid() {
            return Err(needs_review());
        }
        self.change_route(reference, false, Phase::Armed, |current, route, _| {
            current.phases[route] = Phase::Finished;
            Ok(Some(successor))
        })
    }

    fn change_route(
        &self,
        reference: &ConnectionRef,
        active: bool,
        phase: Phase,
        update: impl FnOnce(&mut Current, usize, Budget) -> io::Result<Option<Receiver>>,
    ) -> io::Result<()> {
        self.store.change(|index| {
            let stored = index
                .get_mut(&self.job)
                .filter(|stored| self.matches(stored, active))
                .ok_or_else(needs_review)?;
            let route = stored
                .grant
                .routes
                .iter()
                .position(|route| &route.reference == reference)
                .ok_or_else(needs_review)?;
            let current = stored.current.as_mut().ok_or_else(needs_review)?;
            if current.phases[route] != phase {
                return Err(needs_review());
            }
            if let Some(receiver) = update(current, route, stored.grant.settings.budget)? {
                let expected = &mut stored.grant.routes[route].receiver;
                if !receiver.same_destination(expected) {
                    return Err(needs_review());
                }
                *expected = receiver;
            }
            Ok(())
        })
    }
}

fn valid_index(index: &Index) -> bool {
    index.len() <= kortty_jobs::MAX_JOBS && index.values().all(Stored::valid)
}

fn read_index(directory: &Directory) -> io::Result<Index> {
    let file = match directory.open_private_file(FILE) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Index::new()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err(needs_review());
    }
    let index = serde_json::from_slice(&bytes)?;
    valid_index(&index)
        .then_some(index)
        .ok_or_else(needs_review)
}

fn needs_review() -> io::Error {
    io::Error::other(
        "The recurring rsync approval is stale, exhausted or unavailable. Review the job again.",
    )
}

fn existing_only() -> io::Error {
    io::Error::other(EXISTING_RECEIVER_REQUIRED)
}

pub(super) const EXISTING_RECEIVER_REQUIRED: &str = "Recurring rsync needs a reviewed existing directory, local file, or protected server file on each route.";

fn uncertain() -> io::Error {
    io::Error::other(
        "The last rsync publication could not be confirmed. Check the destination and its recovery copy.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_server_archive::rsync_tree::Manifest;
    use serde_json::json;

    fn command(path: &std::path::Path) -> SshCommand {
        let program = path.join("rsync");
        let source = path.join("source");
        let inventory = Manifest::default().finish();
        let existing = json!({"delete": true, "include_source_name": false});
        let reference = |id: u64, host: &str| json!({"id": id, "target": {"host": host, "port": 22, "username": "test"}});
        let first = reference(1, "first.example");
        let second = reference(2, "second.example");
        let review = |reference: &serde_json::Value, destination: &str| {
            json!({
                "program": program, "digest": vec![1_u8; 32], "reference": reference,
                "pins": ["SHA256:fixture"],
                "existing": {"destination": destination,
                    "snapshot": ExistingTree { inventory, fingerprint: [2; 32] },
                    "settings": existing},
                "directory_upload": {"source": source, "destination": destination, "inventory": inventory}
            })
        };
        let settings = Settings {
            days: 1,
            firings: 3,
            budget: Budget {
                bytes: 10,
                entries: 5,
                deletes: 3,
            },
        };
        serde_json::from_value(json!({
            "connection": first, "additional_connections": [second],
            "remote": {
                "kind": "rsync_upload", "program": program, "local": source,
                "remote": "/first/tree", "directory": true, "existing": existing,
                "approval": review(&first, "/first/tree"), "standing": {"settings": settings},
                "routes": [{"profile": 2, "destination": "/second/tree",
                            "approval": review(&second, "/second/tree")}]
            }
        }))
        .unwrap()
    }

    fn save(store: &Store, job: &Job<SshCommand>, issued: SystemTime) -> Job<SshCommand> {
        let grant = Grant::new(job, &TimeZone::UTC, issued).unwrap();
        let approval = grant.approval.clone();
        store.swap(job.id(), grant, |_| true).ok().unwrap();
        let mut action = job.action().clone();
        let Some(RemoteAction::RsyncUpload(transfer)) = action.remote.as_mut() else {
            panic!("fixture only uses uploads");
        };
        transfer.standing.as_mut().unwrap().approval = Some(approval);
        job.definition().with_action(action).unwrap()
    }

    #[test]
    fn recurring_rsync_requires_disjoint_remote_receivers_on_the_same_pinned_route() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.canonicalize().unwrap();
        for (destination, another_jump, different_pins, overlaps) in [
            ("/first/tree", false, false, true),
            ("/first/tree/child", false, false, true),
            ("/first", false, false, true),
            ("/first/tree-sibling", false, false, false),
            ("/first/tree", true, false, false),
            ("/first/tree", false, true, false),
        ] {
            let mut action = command(&path);
            let original = action.connection.as_ref().unwrap();
            let additional = &mut action.additional_connections[0];
            additional.target = original.target.clone();
            additional.target.host.make_ascii_uppercase();
            if another_jump {
                additional.jump = Some(LoginTarget {
                    host: "bastion.example".into(),
                    ..original.target.clone()
                });
            }
            let Some(RemoteAction::RsyncUpload(transfer)) = action.remote.as_mut() else {
                panic!("fixture only uses uploads");
            };
            let route = &mut transfer.routes[0];
            let review = route.approval.as_mut().unwrap();
            review.reference = additional.clone();
            if different_pins {
                review.pins = vec!["SHA256:another-fixture".into()];
            }
            route.destination = destination.into();
            let path = RemotePath::try_from(destination.to_owned()).unwrap();
            review.existing.as_mut().unwrap().destination = path.clone();
            review.directory_upload.as_mut().unwrap().destination = path;

            let job = Job::new(
                JobId::new("recurring-overlap").unwrap(),
                "Recurring for two saved SSH profiles",
                action,
            )
            .unwrap()
            .with_schedule(JobSchedule::default());
            let result = Grant::new(&job, &TimeZone::UTC, SystemTime::now());
            if overlaps {
                assert_eq!(result.err().unwrap().to_string(), OVERLAPPING_REMOTE);
            } else {
                assert!(result.is_ok());
            }
        }
    }

    #[test]
    fn recurring_rsync_counts_aggregate_cost_and_reopens_with_the_successor() {
        let private = crate::snippets::tests::Profile::new();
        let path = private.0.canonicalize().unwrap();
        let store = Store::new(path.clone());
        let (issued, first_at, second_at) = times();
        let job = Job::new(
            JobId::new("standing-tree").unwrap(),
            "Standing tree",
            command(&path),
        )
        .unwrap()
        .with_schedule(JobSchedule::default().with_enabled(true));
        let job = save(&store, &job, issued);
        let routes = job.action().targets().collect::<Vec<_>>();
        let first = routes[0];
        let second = routes[1];
        let calendar = |instant| FiringOccurrence::Calendar(instant);
        let lease = store
            .claim(&job, &TimeZone::UTC, calendar(first_at), first_at)
            .unwrap();
        let reopened = Store::new(path);
        assert!(
            reopened
                .claim(&job, &TimeZone::UTC, calendar(first_at), first_at)
                .is_err()
        );
        assert!(lease.arm(first, 0).is_err());
        lease
            .reserve(
                first,
                Budget {
                    bytes: 8,
                    entries: 4,
                    deletes: 0,
                },
            )
            .unwrap();
        assert!(lease.reserve(first, Budget::default()).is_err());
        assert!(
            lease
                .reserve(
                    second,
                    Budget {
                        bytes: 3,
                        ..Budget::default()
                    }
                )
                .is_err()
        );
        lease
            .reserve(
                second,
                Budget {
                    bytes: 2,
                    entries: 1,
                    deletes: 0,
                },
            )
            .unwrap();
        lease.arm(first, 2).unwrap();
        assert_eq!(lease.remaining().unwrap().deletes, 1);
        assert!(lease.arm(second, 2).is_err());
        assert!(
            reopened
                .claim(&job, &TimeZone::UTC, calendar(second_at), second_at)
                .is_err()
        );

        let mut successor = lease.expected_receiver(first).unwrap();
        let Receiver::Remote { snapshot, .. } = &mut successor else {
            panic!("fixture uses remote receivers");
        };
        snapshot.fingerprint = [3; 32];
        assert!(lease.confirm(second, successor.clone()).is_err());
        lease.confirm(first, successor.clone()).unwrap();
        assert!(lease.confirm(first, successor.clone()).is_err());
        let next = reopened
            .claim(&job, &TimeZone::UTC, calendar(second_at), second_at)
            .unwrap();
        assert!(next.expected_receiver(first).unwrap() == successor);
        assert!(lease.require_current().is_err());
    }

    #[test]
    fn recurring_rsync_renewal_can_clear_uncertainty_without_replaying_a_chain_round() {
        let private = crate::snippets::tests::Profile::new();
        let path = private.0.canonicalize().unwrap();
        let store = Store::new(path.clone());
        let (issued, first_at, second_at) = times();
        let ancestor = JobId::new("successful-predecessor").unwrap();
        let dependency = |run: u64, ended| {
            let run = serde_json::from_value(json!(run)).unwrap();
            FiringOccurrence::Predecessors(vec![(ancestor.clone(), run, ended)])
        };
        let job = Job::new(
            JobId::new("standing-chain").unwrap(),
            "Standing chain",
            command(&path),
        )
        .unwrap()
        .with_dependencies(DependencyMode::Chain, vec![ancestor.clone()], None);
        let job = save(&store, &job, issued);
        let first = job.action().targets().next().unwrap();
        let lease = store
            .claim(&job, &TimeZone::UTC, dependency(40, first_at), first_at)
            .unwrap();
        lease.reserve(first, Budget::default()).unwrap();
        lease.arm(first, 0).unwrap();
        drop(lease);
        assert!(
            store
                .claim(&job, &TimeZone::UTC, dependency(41, second_at), second_at)
                .is_err()
        );

        let reviewed_again = save(&store, &job, first_at);
        let reopened = Store::new(path);
        assert!(
            reopened
                .claim(
                    &reviewed_again,
                    &TimeZone::UTC,
                    dependency(40, first_at),
                    second_at
                )
                .is_err()
        );
        assert!(
            reopened
                .claim(
                    &reviewed_again,
                    &TimeZone::UTC,
                    dependency(41, second_at),
                    second_at
                )
                .is_ok()
        );
        assert!(
            reopened
                .claim(&job, &TimeZone::UTC, dependency(42, second_at), second_at)
                .is_err()
        );
    }

    #[test]
    fn recurring_rsync_keeps_retired_chain_proofs_consumed_after_switching_back() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.canonicalize().unwrap();
        let store = Store::new(path.clone());
        let (issued, first_at, second_at) = times();
        let first = JobId::new("first-predecessor").unwrap();
        let second = JobId::new("second-predecessor").unwrap();
        let approve = |predecessor: &JobId| {
            let job = Job::new(
                JobId::new("switching-chain").unwrap(),
                "Recurring switch",
                command(&path),
            )
            .unwrap()
            .with_dependencies(DependencyMode::Chain, vec![predecessor.clone()], None);
            save(&store, &job, issued)
        };
        let proof = |id: &JobId, run: u64, finished| {
            let run = serde_json::from_value(json!(run)).unwrap();
            FiringOccurrence::Predecessors(vec![(id.clone(), run, finished)])
        };

        let job = approve(&first);
        store
            .claim(&job, &TimeZone::UTC, proof(&first, 40, first_at), first_at)
            .unwrap();
        let index = read_index(&Directory::open(&path, false).unwrap()).unwrap();
        let legacy = serde_json::to_value(index.get(job.id()).unwrap()).unwrap();
        assert!(legacy.get("retired_predecessor_at").is_none());
        let legacy: Stored = serde_json::from_value(legacy).unwrap();
        assert!(legacy.latest_predecessors.contains_key(&first));
        assert!(legacy.retired_predecessor_at.is_none());

        let job = approve(&second);
        store
            .claim(&job, &TimeZone::UTC, proof(&second, 30, issued), second_at)
            .unwrap();
        let job = approve(&first);
        let reopened = Store::new(path.clone());
        for run in [40, 42] {
            assert!(
                reopened
                    .claim(
                        &job,
                        &TimeZone::UTC,
                        proof(&first, run, first_at),
                        second_at
                    )
                    .is_err()
            );
        }
        assert!(
            reopened
                .claim(
                    &job,
                    &TimeZone::UTC,
                    proof(&first, 43, second_at),
                    second_at
                )
                .is_ok()
        );
    }

    fn times() -> (SystemTime, SystemTime, SystemTime) {
        let now = SystemTime::now();
        (
            now - Duration::from_secs(3),
            now - Duration::from_secs(2),
            now - Duration::from_secs(1),
        )
    }
}
