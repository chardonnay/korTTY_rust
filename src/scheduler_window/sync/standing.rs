//! Bounded, device-local authorization for recurring scheduled SFTP copies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read as _};
use std::time::{Duration, SystemTime};

use kortty_jobs::{
    DependencyMode, FiringOccurrence, Job, JobId, JobSchedule, RunId, RunRequest, RunTrigger,
};
use serde::{Deserialize, Serialize};

use super::{Lock, Store, nonce, require_running, valid_nonce, wait_claim_lock};
use crate::private_files::Directory;

const FILE: &str = "job-sftp-standing-reviews.json";
const LOCK: &str = ".job-sftp-standing-reviews.lock";
const MAX_INDEX_BYTES: u64 = 4 * 1024 * 1024;
const DAY: u64 = 24 * 60 * 60;
pub(crate) const MAX_DAYS: u16 = 30;
pub(crate) const MAX_FIRINGS: u16 = 128;
pub(crate) const MAX_ACTIONS: u16 = crate::sftp_window::scheduled::MAX_STANDING_ACTIONS as u16;
pub(crate) use crate::sftp_window::scheduled::MAX_STANDING_BYTES as MAX_BYTES;
pub(crate) use kortty_server_archive::privileged_file_replace::MAX_BYTES as MAX_REPLACEMENT_BYTES;

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) days: u16,
    pub(crate) limits: Limits,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            days: 7,
            limits: Limits {
                max_firings: 12,
                max_actions: 64,
                max_bytes: 32 * 1024 * 1024,
            },
        }
    }
}

impl Settings {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !(1..=MAX_DAYS).contains(&self.days) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A recurring SFTP approval must last between 1 and 30 days.",
            ));
        }
        self.limits.validate()
    }

    pub(crate) fn validate_replacement(&self) -> io::Result<()> {
        if !(1..=MAX_DAYS).contains(&self.days) || !self.limits.valid_replacement() {
            return Err(io::Error::other(
                "Recurring file replacement allows up to 30 days, 128 firings and 1 GiB per firing.",
            ));
        }
        Ok(())
    }

    pub(crate) fn expiry(&self, now: SystemTime) -> io::Result<SystemTime> {
        self.validate()?;
        self.expiry_from_days(now)
    }

    fn expiry_from_days(&self, now: SystemTime) -> io::Result<SystemTime> {
        now.checked_add(Duration::from_secs(DAY * u64::from(self.days)))
            .ok_or_else(|| io::Error::other("The recurring SFTP approval expiry is out of range."))
    }
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Limits {
    pub(crate) max_firings: u16,
    pub(crate) max_actions: u16,
    pub(crate) max_bytes: u64,
}

impl Limits {
    fn valid(&self) -> bool {
        (1..=MAX_FIRINGS).contains(&self.max_firings)
            && (1..=MAX_ACTIONS).contains(&self.max_actions)
            && (1..=MAX_BYTES).contains(&self.max_bytes)
    }

    fn valid_replacement(&self) -> bool {
        (1..=MAX_FIRINGS).contains(&self.max_firings)
            && self.max_actions == 1
            && (1..=MAX_REPLACEMENT_BYTES).contains(&self.max_bytes)
    }

    fn validate(&self) -> io::Result<()> {
        if self.valid() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Recurring SFTP limits allow up to 128 firings, 256 actions and 256 MiB per firing.",
            ))
        }
    }

    pub(crate) fn allows(&self, actions: usize, bytes: u64) -> bool {
        self.valid() && actions <= usize::from(self.max_actions) && bytes <= self.max_bytes
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Approval {
    pub(crate) approval: String,
    pub(crate) scope: [u8; 32],
    pub(crate) pins: Vec<String>,
    pub(crate) anchors: [u8; 32],
    issued_at: SystemTime,
    pub(crate) expires_at: SystemTime,
    pub(crate) limits: Limits,
}

impl Approval {
    pub(crate) fn new(
        scope: [u8; 32],
        pins: Vec<String>,
        anchors: [u8; 32],
        expires_at: SystemTime,
        limits: Limits,
    ) -> io::Result<Self> {
        limits.validate()?;
        Self::issue(scope, pins, anchors, expires_at, limits, false)
    }

    pub(in crate::scheduler_window) fn for_replacement(
        scope: [u8; 32],
        pins: Vec<String>,
        fingerprint: [u8; 32],
        settings: Settings,
    ) -> io::Result<Self> {
        settings.validate_replacement()?;
        Self::issue(
            scope,
            pins,
            fingerprint,
            settings.expiry_from_days(SystemTime::now())?,
            settings.limits,
            true,
        )
    }

    fn issue(
        scope: [u8; 32],
        pins: Vec<String>,
        anchors: [u8; 32],
        expires_at: SystemTime,
        limits: Limits,
        replacement: bool,
    ) -> io::Result<Self> {
        let approval = Self {
            approval: nonce()?,
            scope,
            pins,
            anchors,
            issued_at: SystemTime::now(),
            expires_at,
            limits,
        };
        if !approval.valid(replacement) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The recurring SFTP approval route or expiry is invalid.",
            ));
        }
        Ok(approval)
    }

    pub(crate) fn active_at(&self, now: SystemTime) -> bool {
        self.issued_at <= now && now < self.expires_at
    }

    fn valid(&self, replacement: bool) -> bool {
        valid_nonce(&self.approval)
            && if replacement {
                self.anchors != [0; 32] && self.limits.valid_replacement()
            } else {
                self.limits.valid()
            }
            && self
                .expires_at
                .duration_since(self.issued_at)
                .is_ok_and(|duration| {
                    !duration.is_zero()
                        && duration <= Duration::from_secs(DAY * u64::from(MAX_DAYS))
                })
            && (1..=2).contains(&self.pins.len())
            && self.pins.iter().all(|pin| {
                !pin.is_empty() && pin.len() <= 512 && !pin.chars().any(char::is_control)
            })
            && (self.pins.len() == 1 || self.pins[0] != self.pins[1])
    }
}

/// A process epoch separates reused run numbers after a scheduler restart.
#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Firing {
    pub(crate) epoch: [u8; 16],
    pub(crate) run: RunId,
}

impl Firing {
    fn valid(self) -> bool {
        self.epoch != [0; 16] && self.run.value() != 0
    }
}

#[derive(Clone)]
pub(crate) struct Admission {
    firing: Firing,
    job: JobId,
    started_at: SystemTime,
    occurrence: FiringOccurrence,
}

impl Admission {
    pub(crate) fn new<A>(
        firing: Firing,
        job: &Job<A>,
        request: &RunRequest<A>,
        occurrence: &FiringOccurrence,
    ) -> io::Result<Self> {
        if !firing.valid()
            || firing.run != request.id
            || job.id() != &request.job
            || !std::ptr::eq(job.action(), request.action.as_ref())
            || !job.enabled()
        {
            return Err(needs_review());
        }
        let allowed = match (request.trigger, occurrence) {
            (RunTrigger::Scheduled, FiringOccurrence::Calendar(instant)) => {
                job.schedule().is_some_and(JobSchedule::enabled) && *instant <= request.started_at
            }
            (RunTrigger::Dependency, FiringOccurrence::Predecessors(round)) => {
                job.mode() == DependencyMode::Chain
                    && !round.is_empty()
                    && round.len() == job.dependencies().len()
                    && round
                        .iter()
                        .zip(job.dependencies())
                        .all(|((id, run, end), expected)| {
                            id == expected
                                && run.value() != 0
                                && request.started_at.duration_since(*end).is_ok_and(|age| {
                                    job.max_age_minutes().is_none_or(|max| {
                                        age.as_secs() / 60 <= u64::from(max.get())
                                    })
                                })
                        })
            }
            _ => false,
        };
        if !allowed {
            return Err(needs_review());
        }
        Ok(Self {
            firing,
            job: request.job.clone(),
            started_at: request.started_at,
            occurrence: occurrence.clone(),
        })
    }

    pub(crate) fn firing(&self) -> Firing {
        self.firing
    }
}

#[cfg(test)]
pub(in crate::scheduler_window) fn calendar_admission(
    job: &JobId,
    firing: Firing,
    at: SystemTime,
) -> Admission {
    Admission {
        firing,
        job: job.clone(),
        started_at: at,
        occurrence: FiringOccurrence::Calendar(at),
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stored {
    approval: Approval,
    claims: Vec<Firing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replacement: Option<Replacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_calendar: Option<SystemTime>,
    // A Chain round requires every predecessor to finish after the previous firing.
    // Keep the cutoff when an approval or its dependency set changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest_firing_started_at: Option<SystemTime>,
}

impl Stored {
    fn has_capacity(&self) -> bool {
        self.claims.len() < usize::from(self.approval.limits.max_firings)
    }

    fn valid(&self) -> bool {
        self.approval.valid(self.replacement.is_some())
            && self.replacement.as_ref().is_none_or(|state| {
                state.fingerprint != [0; 32]
                    && state
                        .pending
                        .as_ref()
                        .is_none_or(|firing| self.claims.last() == Some(firing))
            })
            && self.claims.len() <= usize::from(self.approval.limits.max_firings)
            && self.claims.iter().all(|claim| claim.valid())
            && self.claims.iter().collect::<BTreeSet<_>>().len() == self.claims.len()
            && self.latest_calendar.is_none_or(|instant| {
                self.latest_firing_started_at
                    .is_some_and(|started_at| instant <= started_at)
            })
    }

    fn occurrence_is_new(&self, admission: &Admission) -> bool {
        match &admission.occurrence {
            FiringOccurrence::Calendar(instant) => {
                self.latest_calendar.is_none_or(|latest| *instant > latest)
            }
            FiringOccurrence::Predecessors(round) => self
                .latest_firing_started_at
                .is_none_or(|latest| round.iter().all(|(_, _, finished)| *finished > latest)),
        }
    }

    fn can_admit_replacement(&self, job: &JobId, admission: &Admission) -> bool {
        &admission.job == job
            && self.approval.active_at(admission.started_at)
            && self.has_capacity()
            && !self.claims.contains(&admission.firing())
            && self.occurrence_is_new(admission)
            && !matches!(&admission.occurrence, FiringOccurrence::Calendar(at) if *at < self.approval.issued_at)
    }

    fn admit(&mut self, admission: &Admission) -> io::Result<()> {
        if !self.occurrence_is_new(admission) {
            return Err(needs_review());
        }
        if let FiringOccurrence::Calendar(instant) = &admission.occurrence {
            self.latest_calendar = Some(*instant);
        }
        self.latest_firing_started_at = self
            .latest_firing_started_at
            .max(Some(admission.started_at));
        self.claims.push(admission.firing());
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    fingerprint: [u8; 32],
    pending: Option<Firing>,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ReplacementApproval {
    pub(in crate::scheduler_window) approval: Approval,
    pub(crate) fingerprint: [u8; 32],
}

type Index = BTreeMap<JobId, Stored>;

impl Store {
    pub(crate) fn load_standing(
        &self,
        job: &JobId,
        approval: Option<&str>,
        scope: [u8; 32],
        now: SystemTime,
    ) -> io::Result<Approval> {
        let stored = self.load_current_standing(job, approval, scope, now)?;
        if stored.replacement.is_some() {
            return Err(needs_review());
        }
        Ok(stored.approval)
    }

    pub(in crate::scheduler_window) fn load_replacement(
        &self,
        job: &JobId,
        approval: Option<&str>,
        scope: [u8; 32],
        now: SystemTime,
        admission: Option<&Admission>,
    ) -> io::Result<ReplacementApproval> {
        let stored = self.load_current_standing(job, approval, scope, now)?;
        if admission.is_some_and(|admission| !stored.can_admit_replacement(job, admission)) {
            return Err(needs_review());
        }
        let state = stored.replacement.ok_or_else(needs_review)?;
        if state.pending.is_some() {
            return Err(io::Error::other(
                "The previous file replacement is unconfirmed. Review the destination again.",
            ));
        }
        Ok(ReplacementApproval {
            approval: stored.approval,
            fingerprint: state.fingerprint,
        })
    }

    fn load_current_standing(
        &self,
        job: &JobId,
        approval: Option<&str>,
        scope: [u8; 32],
        now: SystemTime,
    ) -> io::Result<Stored> {
        let approval = approval.ok_or_else(needs_review)?;
        self.standing_snapshot()?
            .remove(job)
            .filter(|stored| {
                stored.approval.approval == approval
                    && stored.approval.scope == scope
                    && stored.approval.active_at(now)
                    && stored.has_capacity()
            })
            .ok_or_else(needs_review)
    }

    pub(crate) fn swap_standing(
        &self,
        job: &JobId,
        approval: Approval,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<Option<Stored>> {
        self.swap_standing_receipt(job, approval, None, keep)
    }

    pub(in crate::scheduler_window) fn swap_replacement(
        &self,
        job: &JobId,
        approval: Approval,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<Option<Stored>> {
        let replacement = Replacement {
            fingerprint: approval.anchors,
            pending: None,
        };
        self.swap_standing_receipt(job, approval, Some(replacement), keep)
    }

    fn swap_standing_receipt(
        &self,
        job: &JobId,
        approval: Approval,
        replacement: Option<Replacement>,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<Option<Stored>> {
        if !approval.valid(replacement.is_some()) || !approval.active_at(SystemTime::now()) {
            return Err(needs_review());
        }
        self.change_standing(None, |index| {
            index.retain(|id, _| id == job || keep(id));
            let prior = index.get(job);
            let latest_calendar = prior.and_then(|stored| stored.latest_calendar);
            let latest_firing_started_at = prior.and_then(|stored| stored.latest_firing_started_at);
            Ok(index.insert(
                job.clone(),
                Stored {
                    approval,
                    claims: Vec::new(),
                    replacement,
                    latest_calendar,
                    latest_firing_started_at,
                },
            ))
        })
    }

    pub(crate) fn restore_standing(
        &self,
        job: &JobId,
        replacement: &Approval,
        prior: Option<Stored>,
    ) -> io::Result<()> {
        self.change_standing(None, |index| {
            if !index
                .get(job)
                .is_some_and(|stored| stored.approval == *replacement && stored.claims.is_empty())
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

    pub(crate) fn claim_standing(
        &self,
        job: &JobId,
        expected: &Approval,
        admission: &Admission,
        actions: usize,
        bytes: u64,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        require_running(stopped)?;
        if &admission.job != job
            || !expected.active_at(admission.started_at)
            || !expected.limits.allows(actions, bytes)
        {
            return Err(needs_review());
        }
        self.change_standing(Some(stopped), |index| {
            require_running(stopped)?;
            let stored = index
                .get_mut(job)
                .filter(|stored| {
                    stored.approval == *expected
                        && stored.replacement.is_none()
                        && stored.approval.active_at(SystemTime::now())
                        && stored.has_capacity()
                        && !stored.claims.contains(&admission.firing())
                })
                .ok_or_else(needs_review)?;
            stored.admit(admission)
        })
    }

    pub(crate) fn require_standing_claimed(
        &self,
        job: &JobId,
        expected: &Approval,
        firing: Firing,
        now: SystemTime,
    ) -> io::Result<()> {
        self.standing_snapshot()?
            .remove(job)
            .filter(|stored| {
                stored.approval == *expected
                    && stored.replacement.is_none()
                    && stored.approval.active_at(now)
                    && stored.claims.contains(&firing)
            })
            .map(|_| ())
            .ok_or_else(needs_review)
    }

    pub(in crate::scheduler_window) fn arm_replacement(
        &self,
        job: &JobId,
        expected: &ReplacementApproval,
        admission: &Admission,
        bytes: u64,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        if bytes > expected.approval.limits.max_bytes {
            return Err(needs_review());
        }
        self.change_standing(Some(stopped), |index| {
            require_running(stopped)?;
            let stored = index
                .get_mut(job)
                .filter(|stored| {
                    stored.approval == expected.approval
                        && stored.approval.active_at(SystemTime::now())
                        && stored.can_admit_replacement(job, admission)
                        && stored.replacement.as_ref().is_some_and(|state| {
                            state.fingerprint == expected.fingerprint && state.pending.is_none()
                        })
                })
                .ok_or_else(needs_review)?;
            stored.admit(admission)?;
            stored
                .replacement
                .as_mut()
                .ok_or_else(needs_review)?
                .pending = Some(admission.firing());
            Ok(())
        })
    }

    pub(in crate::scheduler_window) fn require_armed_replacement(
        &self,
        job: &JobId,
        expected: &ReplacementApproval,
        firing: Firing,
    ) -> io::Result<()> {
        self.standing_snapshot()?
            .get(job)
            .filter(|stored| stored.approval.active_at(SystemTime::now()))
            .and_then(|stored| armed_replacement(stored, expected, firing))
            .map(|_| ())
            .ok_or_else(needs_review)
    }

    /// A held successor advances the receiver. A confirmed rollback keeps it.
    pub(in crate::scheduler_window) fn finish_replacement(
        &self,
        job: &JobId,
        expected: &ReplacementApproval,
        firing: Firing,
        successor: Option<[u8; 32]>,
    ) -> io::Result<()> {
        if successor.is_some_and(|value| value == [0; 32] || value == expected.fingerprint) {
            return Err(needs_review());
        }
        self.change_standing(Some(&|| false), |index| {
            let stored = index.get_mut(job).ok_or_else(needs_review)?;
            armed_replacement(stored, expected, firing).ok_or_else(needs_review)?;
            let state = stored.replacement.as_mut().ok_or_else(needs_review)?;
            state.fingerprint = successor.unwrap_or(expected.fingerprint);
            state.pending = None;
            Ok(())
        })
    }

    fn standing_snapshot(&self) -> io::Result<Index> {
        read_index(&Directory::open(&self.0, false)?)
    }

    fn change_standing<T>(
        &self,
        stopped: Option<&dyn Fn() -> bool>,
        change: impl FnOnce(&mut Index) -> io::Result<T>,
    ) -> io::Result<T> {
        let directory = Directory::open(&self.0, false)?;
        let file = directory.open_lock(LOCK)?;
        let _lock = if let Some(stopped) = stopped {
            wait_claim_lock(file, stopped)?
        } else {
            file.try_lock().map_err(io::Error::other)?;
            Lock(file)
        };
        let mut index = read_index(&directory)?;
        let result = change(&mut index)?;
        if !valid_index(&index) {
            return Err(invalid_index());
        }
        let bytes = serde_json::to_vec(&index)?;
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(invalid_index());
        }
        directory.replace_file(FILE, &bytes)?;
        directory.sync()?;
        Ok(result)
    }
}

fn armed_replacement<'a>(
    stored: &'a Stored,
    expected: &ReplacementApproval,
    firing: Firing,
) -> Option<&'a Replacement> {
    stored.replacement.as_ref().filter(|state| {
        stored.approval == expected.approval
            && state.fingerprint == expected.fingerprint
            && state.pending == Some(firing)
    })
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
        return Err(invalid_index());
    }
    let index: Index = serde_json::from_slice(&bytes)?;
    if !valid_index(&index) {
        return Err(invalid_index());
    }
    Ok(index)
}

fn valid_index(index: &Index) -> bool {
    index.len() <= kortty_jobs::MAX_JOBS && index.values().all(Stored::valid)
}

fn invalid_index() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The recurring SFTP approval store is invalid or exceeds its limits.",
    )
}

fn needs_review() -> io::Error {
    io::Error::other("This scheduled SFTP copy needs a current recurring approval.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler_window::tests::Profile;
    use kortty_jobs::{JobGraph, RunOutcome, Scheduler};
    use std::num::NonZeroU32;

    #[test]
    fn recurring_protected_replacement_persists_only_confirmed_receivers() {
        let profile = Profile::temporary();
        let path = profile.file();
        let directory = path.parent().unwrap();
        let job = JobId::new("protected-replacement").unwrap();
        let approval = Approval::for_replacement(
            [2; 32],
            vec!["Pinned server".into()],
            [3; 32],
            Settings {
                days: 1,
                limits: Limits {
                    max_firings: 2,
                    max_actions: 1,
                    max_bytes: MAX_REPLACEMENT_BYTES,
                },
            },
        )
        .unwrap();
        let now = approval.issued_at;
        let first = calendar_admission(&job, firing(1, 1), now);
        let second = calendar_admission(&job, firing(2, 1), now + Duration::from_secs(1));
        let load = |store: &Store, admission| {
            store.load_replacement(
                &job,
                Some(&approval.approval),
                approval.scope,
                now,
                admission,
            )
        };
        let store = Store::new(directory);
        store
            .swap_replacement(&job, approval.clone(), |_| true)
            .unwrap();
        assert!(
            store
                .load_standing(&job, Some(&approval.approval), approval.scope, now)
                .is_err()
        );
        let initial = load(&store, Some(&first)).unwrap();
        assert_eq!(initial.fingerprint, [3; 32]);
        assert!(
            store
                .arm_replacement(&job, &initial, &first, MAX_REPLACEMENT_BYTES + 1, &|| false)
                .is_err()
        );
        store
            .arm_replacement(&job, &initial, &first, MAX_REPLACEMENT_BYTES, &|| false)
            .unwrap();
        assert!(load(&store, Some(&second)).is_err());
        let reopened = Store::new(directory);
        assert!(load(&reopened, Some(&second)).is_err());
        assert!(
            reopened
                .finish_replacement(&job, &initial, second.firing(), Some([4; 32]))
                .is_err()
        );
        assert!(
            reopened
                .finish_replacement(&job, &initial, first.firing(), Some([0; 32]))
                .is_err()
        );
        reopened
            .require_armed_replacement(&job, &initial, first.firing())
            .unwrap();
        reopened
            .finish_replacement(&job, &initial, first.firing(), Some([4; 32]))
            .unwrap();
        assert!(load(&reopened, Some(&first)).is_err());
        let same_calendar = calendar_admission(&job, firing(2, 2), now);
        assert!(load(&reopened, Some(&same_calendar)).is_err());
        let current = load(&reopened, Some(&second)).unwrap();
        assert_eq!(current.fingerprint, [4; 32]);
        assert!(
            reopened
                .arm_replacement(&job, &initial, &second, 1, &|| false)
                .is_err()
        );
        reopened
            .arm_replacement(&job, &current, &second, 1, &|| false)
            .unwrap();
        reopened
            .finish_replacement(&job, &current, second.firing(), None)
            .unwrap();
        assert!(load(&reopened, None).is_err());
    }

    fn approval() -> Approval {
        let settings = Settings {
            limits: Limits {
                max_firings: 2,
                ..Settings::default().limits
            },
            ..Settings::default()
        };
        Approval::new(
            [1; 32],
            vec!["pinned route".into()],
            [2; 32],
            settings.expiry(SystemTime::now()).unwrap(),
            settings.limits,
        )
        .unwrap()
    }

    fn firing(epoch: u8, run: u64) -> Firing {
        Firing {
            epoch: [epoch; 16],
            run: serde_json::from_value(run.into()).unwrap(),
        }
    }

    #[test]
    fn recurring_sftp_chain_admission_matches_the_active_predecessor_proof() {
        let upstream = JobId::new("upstream").unwrap();
        let downstream = JobId::new("sftp-chain").unwrap();
        let mut scheduler = Scheduler::new(
            JobGraph::new(vec![
                Job::new(upstream.clone(), "upstream", "prepare").unwrap(),
                Job::new(downstream, "sftp-chain", "copy")
                    .unwrap()
                    .with_dependencies(
                        DependencyMode::Chain,
                        vec![upstream.clone()],
                        NonZeroU32::new(1),
                    ),
            ])
            .unwrap(),
        );
        let start = SystemTime::now();
        let first = scheduler.start_manual(&upstream, start).unwrap();
        let mut launched = scheduler
            .complete(first.started[0].id, RunOutcome::Success, start)
            .unwrap();
        let request = &mut launched.started[0];
        let FiringOccurrence::Predecessors(round) = scheduler
            .active_firing_occurrence(request)
            .cloned()
            .unwrap()
        else {
            panic!("expected an active predecessor round");
        };
        let job = scheduler.graph().job(&request.job).unwrap();
        let firing = Firing {
            epoch: [1; 16],
            run: request.id,
        };
        let occurrence = FiringOccurrence::Predecessors(round.clone());
        assert!(Admission::new(firing, job, request, &occurrence).is_ok());
        assert!(Admission::new(firing, job, request, &FiringOccurrence::Calendar(start)).is_err());

        let check = |id, finished| {
            Admission::new(
                firing,
                job,
                request,
                &FiringOccurrence::Predecessors(vec![(id, round[0].1, finished)]),
            )
        };
        assert!(check(JobId::new("foreign").unwrap(), start).is_err());
        assert!(check(upstream.clone(), start + Duration::from_millis(1)).is_err());
        // The Scheduler measures the freshness limit in complete minutes.
        assert!(check(upstream.clone(), start - Duration::from_secs(119)).is_ok());
        assert!(check(upstream, start - Duration::from_secs(120)).is_err());

        request.trigger = RunTrigger::Manual;
        assert!(Admission::new(firing, job, request, &occurrence).is_err());
        request.trigger = RunTrigger::Cascade;
        assert!(Admission::new(firing, job, request, &occurrence).is_err());
    }

    #[test]
    fn recurring_sftp_chain_rejects_replay_after_reapproval_and_changes_to_predecessors() {
        let profile = Profile::temporary();
        let path = profile.file();
        let directory = path.parent().unwrap();
        let job = JobId::new("sftp-chain").unwrap();
        let source = JobId::new("original-predecessor").unwrap();
        let new_source = JobId::new("new-predecessor").unwrap();
        let grant = approval();
        let later = grant.issued_at + Duration::from_secs(5);
        let round = |claim, source: &JobId, finished_at, started_at| Admission {
            firing: claim,
            job: job.clone(),
            started_at,
            occurrence: FiringOccurrence::Predecessors(vec![(
                source.clone(),
                firing(1, 1).run,
                finished_at,
            )]),
        };
        let original = round(firing(1, 10), &source, later, later);
        {
            let store = Store::new(directory);
            store.swap_standing(&job, grant.clone(), |_| true).unwrap();
            store
                .claim_standing(&job, &grant, &original, 1, 0, &|| false)
                .unwrap();
        }
        let reopened = Store::new(directory);
        let retry = round(firing(2, 1), &source, later, later + Duration::from_secs(1));
        assert!(
            reopened
                .claim_standing(&job, &grant, &retry, 1, 0, &|| false)
                .is_err()
        );

        let reapproved = Approval {
            scope: [3; 32],
            ..approval()
        };
        reopened
            .swap_standing(&job, reapproved.clone(), |_| true)
            .unwrap();
        assert!(
            reopened
                .claim_standing(&job, &reapproved, &retry, 1, 0, &|| false)
                .is_err()
        );
        let different = round(
            firing(2, 1),
            &new_source,
            later,
            later + Duration::from_secs(1),
        );
        assert!(
            reopened
                .claim_standing(&job, &reapproved, &different, 1, 0, &|| false)
                .is_err()
        );

        let later = later + Duration::from_secs(2);
        let calendar = calendar_admission(&job, firing(2, 2), later);
        reopened
            .claim_standing(&job, &reapproved, &calendar, 1, 0, &|| false)
            .unwrap();
        let stale = round(firing(2, 3), &new_source, later, later);
        assert!(
            reopened
                .claim_standing(&job, &reapproved, &stale, 1, 0, &|| false)
                .is_err()
        );
        let next_at = later + Duration::from_millis(1);
        let next = round(firing(2, 3), &new_source, next_at, next_at);
        reopened
            .claim_standing(&job, &reapproved, &next, 1, 0, &|| false)
            .unwrap();
        reopened
            .require_standing_claimed(&job, &reapproved, next.firing(), SystemTime::now())
            .unwrap();
    }

    #[test]
    fn legacy_recurring_sftp_calendar_claims_keep_their_approval_and_limit() {
        let profile = Profile::temporary();
        let path = profile.file();
        let directory = path.parent().unwrap();
        let job = JobId::new("recurring-copy").unwrap();
        let approval = approval();
        let legacy = firing(1, 4);
        let data = serde_json::to_vec(&BTreeMap::from([(
            &job,
            serde_json::json!({ "approval": approval, "claims": [legacy] }),
        )]))
        .unwrap();
        Directory::open(directory, false)
            .unwrap()
            .replace_file(FILE, &data)
            .unwrap();
        let store = Store::new(directory);
        let loaded = store
            .load_standing(
                &job,
                Some(&approval.approval),
                approval.scope,
                SystemTime::now(),
            )
            .unwrap();
        store
            .require_standing_claimed(&job, &loaded, legacy, SystemTime::now())
            .unwrap();
        let continuing = calendar_admission(&job, firing(2, 4), SystemTime::now());
        store
            .claim_standing(&job, &loaded, &continuing, 0, 0, &|| false)
            .unwrap();
        assert!(
            store
                .load_standing(
                    &job,
                    Some(&approval.approval),
                    approval.scope,
                    SystemTime::now()
                )
                .is_err()
        );
    }

    #[test]
    fn recurring_sftp_accepts_a_queued_calendar_start_during_its_approval() {
        let profile = Profile::temporary();
        let path = profile.file();
        let store = Store::new(path.parent().unwrap());
        let job = JobId::new("recurring-copy").unwrap();
        let approval = approval();
        let due = approval.issued_at - Duration::from_secs(1);
        let mut queued = calendar_admission(&job, firing(1, 1), due);
        queued.started_at = approval.issued_at;
        store
            .swap_standing(&job, approval.clone(), |_| true)
            .unwrap();
        store
            .claim_standing(&job, &approval, &queued, 0, 0, &|| false)
            .unwrap();

        let mut replay = calendar_admission(&job, firing(2, 1), due);
        replay.started_at = queued.started_at;
        assert!(
            store
                .claim_standing(&job, &approval, &replay, 0, 0, &|| false)
                .is_err()
        );
    }

    #[test]
    fn recurring_sftp_claims_survive_reopen_and_reject_replay_and_exhaustion() {
        let profile = Profile::temporary();
        let path = profile.file();
        let directory = path.parent().unwrap();
        let job = JobId::new("recurring-copy").unwrap();
        let approval = approval();
        let key = Some(approval.approval.as_str());
        let start = approval.issued_at + Duration::from_secs(1);
        let first = calendar_admission(&job, firing(1, 41), start);
        {
            let store = Store::new(directory);
            assert!(
                store
                    .swap_standing(&job, approval.clone(), |_| true)
                    .unwrap()
                    .is_none()
            );
            let loaded = store
                .load_standing(&job, key, approval.scope, SystemTime::now())
                .unwrap();
            assert!(loaded == approval);
            assert!(
                store
                    .require_standing_claimed(&job, &approval, first.firing(), SystemTime::now())
                    .is_err()
            );
            store
                .claim_standing(&job, &approval, &first, 0, 0, &|| false)
                .unwrap();
        }

        let reopened = Store::new(directory);
        reopened
            .require_standing_claimed(&job, &approval, first.firing(), SystemTime::now())
            .unwrap();
        assert!(
            reopened
                .claim_standing(&job, &approval, &first, 0, 0, &|| false)
                .is_err()
        );
        let same_occurrence = calendar_admission(&job, firing(2, 41), start);
        assert!(
            reopened
                .claim_standing(&job, &approval, &same_occurrence, 0, 0, &|| false)
                .is_err()
        );
        // A new scheduler may legitimately reuse the process-local run number.
        let second = calendar_admission(&job, firing(2, 41), start + Duration::from_secs(1));
        reopened
            .claim_standing(&job, &approval, &second, 0, 0, &|| false)
            .unwrap();
        assert!(
            reopened
                .load_standing(&job, key, approval.scope, SystemTime::now())
                .is_err()
        );
        reopened
            .require_standing_claimed(&job, &approval, second.firing(), SystemTime::now())
            .unwrap();
        assert!(
            reopened
                .require_standing_claimed(&job, &approval, second.firing(), approval.expires_at)
                .is_err()
        );
        let third = calendar_admission(&job, firing(2, 42), start + Duration::from_secs(2));
        assert!(
            reopened
                .claim_standing(&job, &approval, &third, 0, 0, &|| false)
                .is_err()
        );

        let replacement = Approval {
            approval: nonce().unwrap(),
            ..approval.clone()
        };
        let prior = reopened
            .swap_standing(&job, replacement.clone(), |_| true)
            .unwrap();
        assert!(
            reopened
                .require_standing_claimed(&job, &approval, second.firing(), SystemTime::now())
                .is_err()
        );
        assert!(
            reopened
                .claim_standing(&job, &replacement, &same_occurrence, 0, 0, &|| false)
                .is_err()
        );
        reopened
            .restore_standing(&job, &replacement, prior)
            .unwrap();
        reopened
            .require_standing_claimed(&job, &approval, second.firing(), SystemTime::now())
            .unwrap();
        assert!(
            reopened
                .load_standing(&job, key, approval.scope, SystemTime::now())
                .is_err()
        );
    }

    #[test]
    fn recurring_sftp_rejects_invalid_limits_and_cancelled_claims_without_writing() {
        let profile = Profile::temporary();
        let path = profile.file();
        let directory = path.parent().unwrap();
        let store = Store::new(directory);
        let job = JobId::new("recurring-copy").unwrap();
        let approval = approval();
        let invalid = Approval {
            limits: Limits {
                max_actions: 0,
                ..approval.limits
            },
            ..approval.clone()
        };
        assert!(store.swap_standing(&job, invalid, |_| true).is_err());
        assert!(!directory.join(FILE).exists());
        assert!(!directory.join(LOCK).exists());
        assert!(!directory.join(super::super::FILE).exists());

        store
            .swap_standing(&job, approval.clone(), |_| true)
            .unwrap();
        let saved = std::fs::read(directory.join(FILE)).unwrap();
        let first = calendar_admission(&job, firing(1, 41), SystemTime::now());
        assert_eq!(
            store
                .claim_standing(&job, &approval, &first, 0, 0, &|| true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(
            store
                .claim_standing(
                    &job,
                    &approval,
                    &first,
                    usize::from(approval.limits.max_actions) + 1,
                    0,
                    &|| false
                )
                .is_err()
        );
        assert!(
            store
                .claim_standing(
                    &job,
                    &approval,
                    &first,
                    0,
                    approval.limits.max_bytes + 1,
                    &|| false
                )
                .is_err()
        );
        assert_eq!(std::fs::read(directory.join(FILE)).unwrap(), saved);
        store
            .claim_standing(&job, &approval, &first, 0, 0, &|| false)
            .unwrap();
        assert!(!directory.join(super::super::FILE).exists());
    }
}
