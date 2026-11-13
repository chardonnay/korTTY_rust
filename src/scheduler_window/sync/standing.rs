//! Bounded, device-local authorization for recurring scheduled SFTP copies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read as _};
use std::time::{Duration, SystemTime};

use kortty_jobs::{JobId, RunId};
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

    pub(crate) fn expiry(&self, now: SystemTime) -> io::Result<SystemTime> {
        self.validate()?;
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
        let approval = Self {
            approval: nonce()?,
            scope,
            pins,
            anchors,
            issued_at: SystemTime::now(),
            expires_at,
            limits,
        };
        if !approval.valid() {
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

    fn valid(&self) -> bool {
        valid_nonce(&self.approval)
            && self.limits.valid()
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Stored {
    approval: Approval,
    claims: Vec<Firing>,
}

impl Stored {
    fn has_capacity(&self) -> bool {
        self.claims.len() < usize::from(self.approval.limits.max_firings)
    }

    fn valid(&self) -> bool {
        self.approval.valid()
            && self.claims.len() <= usize::from(self.approval.limits.max_firings)
            && self.claims.iter().all(|claim| claim.valid())
            && self.claims.iter().collect::<BTreeSet<_>>().len() == self.claims.len()
    }
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
        let approval = approval.ok_or_else(needs_review)?;
        self.standing_snapshot()?
            .remove(job)
            .filter(|stored| {
                stored.approval.approval == approval
                    && stored.approval.scope == scope
                    && stored.approval.active_at(now)
                    && stored.has_capacity()
            })
            .map(|stored| stored.approval)
            .ok_or_else(needs_review)
    }

    pub(crate) fn swap_standing(
        &self,
        job: &JobId,
        approval: Approval,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<Option<Stored>> {
        if !approval.valid() || !approval.active_at(SystemTime::now()) {
            return Err(needs_review());
        }
        self.change_standing(None, |index| {
            index.retain(|id, _| id == job || keep(id));
            Ok(index.insert(
                job.clone(),
                Stored {
                    approval,
                    claims: Vec::new(),
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
        firing: Firing,
        actions: usize,
        bytes: u64,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        require_running(stopped)?;
        if !firing.valid() || !expected.limits.allows(actions, bytes) {
            return Err(needs_review());
        }
        self.change_standing(Some(stopped), |index| {
            require_running(stopped)?;
            let stored = index
                .get_mut(job)
                .filter(|stored| {
                    stored.approval == *expected
                        && stored.approval.active_at(SystemTime::now())
                        && stored.has_capacity()
                        && !stored.claims.contains(&firing)
                })
                .ok_or_else(needs_review)?;
            stored.claims.push(firing);
            Ok(())
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
                    && stored.approval.active_at(now)
                    && stored.claims.contains(&firing)
            })
            .map(|_| ())
            .ok_or_else(needs_review)
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
    fn recurring_sftp_claims_survive_reopen_and_reject_replay_and_exhaustion() {
        let profile = Profile::temporary();
        let path = profile.file();
        let directory = path.parent().unwrap();
        let job = JobId::new("recurring-copy").unwrap();
        let approval = approval();
        let key = Some(approval.approval.as_str());
        let first = firing(1, 41);
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
                    .require_standing_claimed(&job, &approval, first, SystemTime::now())
                    .is_err()
            );
            store
                .claim_standing(&job, &approval, first, 0, 0, &|| false)
                .unwrap();
        }

        let reopened = Store::new(directory);
        reopened
            .require_standing_claimed(&job, &approval, first, SystemTime::now())
            .unwrap();
        assert!(
            reopened
                .claim_standing(&job, &approval, first, 0, 0, &|| false)
                .is_err()
        );
        // A new scheduler may legitimately reuse the process-local run number.
        let second = firing(2, 41);
        reopened
            .claim_standing(&job, &approval, second, 0, 0, &|| false)
            .unwrap();
        assert!(
            reopened
                .load_standing(&job, key, approval.scope, SystemTime::now())
                .is_err()
        );
        reopened
            .require_standing_claimed(&job, &approval, second, SystemTime::now())
            .unwrap();
        assert!(
            reopened
                .require_standing_claimed(&job, &approval, second, approval.expires_at)
                .is_err()
        );
        assert!(
            reopened
                .claim_standing(&job, &approval, firing(2, 42), 0, 0, &|| false)
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
                .require_standing_claimed(&job, &approval, second, SystemTime::now())
                .is_err()
        );
        reopened
            .restore_standing(&job, &replacement, prior)
            .unwrap();
        reopened
            .require_standing_claimed(&job, &approval, second, SystemTime::now())
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
        let first = firing(1, 41);
        assert_eq!(
            store
                .claim_standing(&job, &approval, first, 0, 0, &|| true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(
            store
                .claim_standing(
                    &job,
                    &approval,
                    first,
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
                    first,
                    0,
                    approval.limits.max_bytes + 1,
                    &|| false
                )
                .is_err()
        );
        assert_eq!(std::fs::read(directory.join(FILE)).unwrap(), saved);
        store
            .claim_standing(&job, &approval, first, 0, 0, &|| false)
            .unwrap();
        assert!(!directory.join(super::super::FILE).exists());
    }
}
