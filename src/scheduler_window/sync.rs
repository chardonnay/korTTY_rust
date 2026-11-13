//! Device-local authorization for scheduled folder copies.

use std::collections::BTreeMap;
use std::fs::{File, TryLockError};
use std::io::{self, Read as _};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use kortty_jobs::{
    Cancellation, DependencyMode, Job, JobId, JobSchedule, RunId, RunRequest, RunTrigger, TimeZone,
};
use kortty_ssh::{Endpoint, HostKey, TrustStore};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::private_files::{Directory, nonce};
use crate::sftp_window::{Side, scheduled};

use super::{ConnectionRef, RemotePath, SshCommand, SudoMode};

pub(crate) mod standing;
mod ui;
pub(crate) use ui::PreviewRequest;
pub(super) use ui::{CascadeReview, DraftReview};

const FILE: &str = "job-sftp-reviews.json";
const LOCK: &str = ".job-sftp-reviews.lock";
const CLAIM_WAIT: Duration = Duration::from_secs(2);
const CLAIM_POLL: Duration = Duration::from_millis(15);
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const CHAIN_PREDECESSORS: &str = "Reviewed SFTP Chain mode requires at least one predecessor.";

#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    Upload,
    Download,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Copy {
    pub(crate) local: PathBuf,
    pub(crate) remote: RemotePath,
    pub(crate) direction: Direction,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) excludes: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) replace_changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) standing: Option<standing::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approval: Option<String>,
}

impl Copy {
    pub(super) fn validate_replacement(&self, privileged: bool) -> Result<(), &'static str> {
        if self.replace_changed
            && (self.direction != Direction::Download || privileged || self.standing.is_some())
        {
            return Err("Changed-file replacement requires an ordinary one-use SFTP download.");
        }
        Ok(())
    }

    pub(super) fn validate_job_schedule(
        &self,
        schedule: Option<&JobSchedule>,
        mode: DependencyMode,
        predecessors: &[JobId],
        privileged: bool,
    ) -> Result<(), &'static str> {
        self.validate_replacement(privileged)?;
        if self.standing.is_none() {
            return validate_scheduling(schedule, mode, predecessors, privileged);
        }
        if privileged || mode != DependencyMode::Gate || !schedule.is_some_and(JobSchedule::enabled)
        {
            return Err(
                "Recurring SFTP copies require an enabled calendar and Condition dependency mode.",
            );
        }
        Ok(())
    }

    pub(super) fn standing_scope(
        &self,
        job: &Job<SshCommand>,
        connection: &ConnectionRef,
        zone: &TimeZone,
    ) -> io::Result<[u8; 32]> {
        let settings = self.standing.as_ref().ok_or_else(needs_review)?;
        settings.validate()?;
        let zone = zone.iana_name().ok_or_else(|| {
            io::Error::other("A named scheduler time zone is required for recurring SFTP copies.")
        })?;
        let scope = self.conditioned_scope(
            connection,
            job.schedule(),
            job.mode(),
            job.dependencies(),
            job.max_age_minutes(),
        )?;
        let bytes = serde_json::to_vec(&(
            "kortty-scheduled-sftp-standing-v1",
            job.id(),
            scope,
            zone,
            settings,
        ))?;
        let digest = HashAlgorithm::Sha256
            .digest(&bytes)
            .map_err(io::Error::other)?;
        Ok(digest.try_into().expect("SHA-256 produces 32 bytes"))
    }

    pub(crate) fn comparison(&self) -> scheduled::Comparison<'_> {
        scheduled::Comparison {
            local: self.local.clone(),
            remote: self.remote.as_str().as_bytes().to_vec(),
            excludes: &self.excludes,
            replace_changed: self.replace_changed,
            to: match self.direction {
                Direction::Upload => Side::Remote,
                Direction::Download => Side::Local,
            },
        }
    }

    pub(crate) fn scope(
        &self,
        connection: &ConnectionRef,
        schedule: Option<&JobSchedule>,
    ) -> io::Result<[u8; 32]> {
        self.validate_replacement(false).map_err(io::Error::other)?;
        let domain = if self.replace_changed {
            "kortty-scheduled-sftp-replace-changed-v1"
        } else {
            "kortty-scheduled-sftp-v1"
        };
        let scope = serde_json::to_vec(&(
            domain,
            connection,
            &self.local,
            &self.remote,
            self.direction,
            &self.excludes,
            schedule,
        ))?;
        let mut hasher = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        hasher.update(&scope);
        Ok(hasher
            .finalize()
            .as_ref()
            .try_into()
            .expect("SHA-256 produces 32 bytes"))
    }

    pub(super) fn conditioned_scope(
        &self,
        connection: &ConnectionRef,
        schedule: Option<&JobSchedule>,
        mode: DependencyMode,
        predecessors: &[JobId],
        max_age_minutes: Option<NonZeroU32>,
    ) -> io::Result<[u8; 32]> {
        let scope = self.scope(connection, schedule)?;
        let scope = if self.direction == Direction::Download {
            let bytes = serde_json::to_vec(&("kortty-scheduled-sftp-download-creation-v1", scope))?;
            HashAlgorithm::Sha256
                .digest(&bytes)
                .map_err(io::Error::other)?
                .try_into()
                .expect("SHA-256 produces 32 bytes")
        } else {
            scope
        };
        if predecessors.is_empty() {
            if mode == DependencyMode::Chain {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    CHAIN_PREDECESSORS,
                ));
            }
            // No predecessor condition needs an additional scope binding.
            return Ok(scope);
        }
        let bytes = serde_json::to_vec(&(
            "kortty-scheduled-sftp-condition-v1",
            scope,
            mode,
            predecessors,
            max_age_minutes,
        ))?;
        let digest = HashAlgorithm::Sha256
            .digest(&bytes)
            .map_err(io::Error::other)?;
        Ok(digest.try_into().expect("SHA-256 produces 32 bytes"))
    }

    pub(super) fn valid_approval(&self) -> bool {
        self.approval.as_deref().is_none_or(valid_nonce)
    }

    pub(super) fn privileged_scope(
        &self,
        connection: &ConnectionRef,
        schedule: Option<&JobSchedule>,
        sudo: SudoMode,
    ) -> io::Result<[u8; 32]> {
        self.validate_replacement(true).map_err(io::Error::other)?;
        let scope = self.scope(connection, schedule)?;
        let bytes = serde_json::to_vec(&("kortty-scheduled-privileged-tree-v1", scope, sudo))?;
        let mut hasher = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        hasher.update(&bytes);
        Ok(hasher.finalize().as_ref().try_into().expect("SHA-256 size"))
    }
}

pub(super) fn validate_scheduling(
    schedule: Option<&JobSchedule>,
    mode: DependencyMode,
    predecessors: &[JobId],
    privileged: bool,
) -> Result<(), &'static str> {
    validate_schedule(schedule)?;
    if privileged {
        if mode != DependencyMode::Gate {
            return Err("Reviewed SFTP copy requires Condition dependency mode.");
        }
        if !predecessors.is_empty() {
            return Err("Reviewed privileged folder transfers require a job without predecessors.");
        }
    } else if mode == DependencyMode::Chain && predecessors.is_empty() {
        return Err(CHAIN_PREDECESSORS);
    }
    Ok(())
}

pub(crate) fn validate_schedule(schedule: Option<&JobSchedule>) -> Result<(), &'static str> {
    if let Some(schedule) = schedule.filter(|schedule| schedule.enabled()) {
        let (from, until) = schedule.active_dates();
        if from.is_none()
            || from != until
            || schedule.fixed_times().count() != 1
            || schedule.interval_minutes().is_some()
        {
            return Err("Reviewed SFTP copy requires a manual run or one date and one fixed time.");
        }
    }
    Ok(())
}

pub(super) fn valid_nonce(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn pins(
    connection: &ConnectionRef,
    trust: &TrustStore,
) -> io::Result<Vec<(Endpoint, HostKey)>> {
    let mut endpoints = connection
        .jump
        .iter()
        .chain(std::iter::once(&connection.target))
        .map(|login| Endpoint::new(&login.host, login.port))
        .collect::<io::Result<Vec<_>>>()?;
    endpoints.sort_unstable();
    endpoints.dedup();
    let pins = trust.selected_pins(&endpoints)?;
    if pins.len() == endpoints.len() {
        Ok(pins)
    } else {
        Err(io::Error::other(
            "Verify the SSH host keys for the saved route before review.",
        ))
    }
}

pub(crate) fn fingerprints(pins: &[(Endpoint, HostKey)]) -> Vec<String> {
    pins.iter()
        .map(|(endpoint, key)| format!("{endpoint} {}", key.fingerprint()))
        .collect()
}

#[derive(Clone)]
pub(crate) struct RunContext {
    pub(crate) store: Store,
    pub(crate) scope: [u8; 32],
    run: RunId,
    job: JobId,
    action: Arc<SshCommand>,
    started_at: SystemTime,
    trigger: RunTrigger,
    cancellation: Cancellation,
    cascade_receipt: Option<Receipt>,
    standing: Option<(standing::Approval, standing::Firing)>,
}

impl RunContext {
    pub(crate) fn permits(&self, request: &RunRequest<SshCommand>) -> bool {
        self.run == request.id
            && self.job == request.job
            && Arc::ptr_eq(&self.action, &request.action)
            && self.started_at == request.started_at
            && self.trigger == request.trigger
            && self.cancellation.shares_signal_with(&request.cancellation)
    }

    pub(crate) fn load_receipt(&self, approval: Option<&str>) -> io::Result<Receipt> {
        if self.standing.is_some() {
            return Err(needs_review());
        }
        let current = self.store.load(&self.job, approval, self.scope)?;
        if self.trigger == RunTrigger::Cascade && self.cascade_receipt.as_ref() != Some(&current) {
            return Err(needs_review());
        }
        Ok(current)
    }

    pub(crate) fn standing_approval(&self) -> Option<&(standing::Approval, standing::Firing)> {
        self.standing.as_ref()
    }
}

pub(crate) struct Review {
    pub(crate) preview: scheduled::Preview,
    pub(crate) pins: Vec<String>,
    pub(crate) standing_roots: Option<[u8; 32]>,
}

#[derive(Clone)]
pub(super) enum SaveApproval {
    OneUse(Receipt),
    Standing(standing::Approval),
}

pub(super) enum ApprovalRollback {
    OneUse(Receipt, Option<Receipt>),
    Standing(standing::Approval, Option<standing::Stored>),
}

impl SaveApproval {
    pub(super) fn nonce(&self) -> &str {
        match self {
            Self::OneUse(receipt) => &receipt.approval,
            Self::Standing(approval) => &approval.approval,
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) approval: String,
    pub(crate) scope: [u8; 32],
    pub(crate) proof: [u8; 32],
    pub(crate) pins: Vec<String>,
    used: bool,
}

impl Receipt {
    pub(super) fn new(scope: [u8; 32], review: &Review) -> io::Result<Self> {
        Self::for_operation(scope, review.preview.proof, review.pins.clone())
    }

    pub(super) fn for_operation(
        scope: [u8; 32],
        proof: [u8; 32],
        pins: Vec<String>,
    ) -> io::Result<Self> {
        Ok(Self {
            approval: nonce()?,
            scope,
            proof,
            pins,
            used: false,
        })
    }

    fn valid(&self) -> bool {
        valid_nonce(&self.approval)
            && (1..=2).contains(&self.pins.len())
            && self
                .pins
                .iter()
                .all(|pin| pin.len() <= 512 && !pin.chars().any(char::is_control))
    }

    fn matches_approval(&self, approval: Option<&str>, scope: [u8; 32]) -> bool {
        Some(self.approval.as_str()) == approval && self.scope == scope && !self.used
    }
}

#[derive(Clone)]
pub(crate) struct Store(PathBuf);

type Index = BTreeMap<JobId, Receipt>;

impl Store {
    pub(super) fn new(profile: &Path) -> Self {
        Self(profile.to_owned())
    }

    pub(super) fn save_approval(
        &self,
        job: &JobId,
        approval: SaveApproval,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<ApprovalRollback> {
        match approval {
            SaveApproval::OneUse(receipt) => {
                let prior = self.swap(job, receipt.clone(), keep)?;
                Ok(ApprovalRollback::OneUse(receipt, prior))
            }
            SaveApproval::Standing(approval) => {
                let prior = self.swap_standing(job, approval.clone(), keep)?;
                Ok(ApprovalRollback::Standing(approval, prior))
            }
        }
    }

    pub(super) fn restore_approval(
        &self,
        job: &JobId,
        rollback: ApprovalRollback,
    ) -> io::Result<()> {
        match rollback {
            ApprovalRollback::OneUse(receipt, prior) => self.restore(job, &receipt, prior),
            ApprovalRollback::Standing(approval, prior) => {
                self.restore_standing(job, &approval, prior)
            }
        }
    }

    pub(super) fn swap(
        &self,
        job: &JobId,
        receipt: Receipt,
        keep: impl Fn(&JobId) -> bool,
    ) -> io::Result<Option<Receipt>> {
        self.change(|index| {
            index.retain(|id, _| id == job || keep(id));
            Ok(index.insert(job.clone(), receipt))
        })
    }

    pub(super) fn restore(
        &self,
        job: &JobId,
        replacement: &Receipt,
        prior: Option<Receipt>,
    ) -> io::Result<()> {
        self.change(|index| {
            if index.get(job) != Some(replacement) {
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

    pub(crate) fn load(
        &self,
        job: &JobId,
        approval: Option<&str>,
        scope: [u8; 32],
    ) -> io::Result<Receipt> {
        self.snapshot()?
            .remove(job)
            .filter(|receipt| receipt.matches_approval(approval, scope))
            .ok_or_else(needs_review)
    }

    pub(crate) fn claim(&self, job: &JobId, expected: &Receipt) -> io::Result<()> {
        self.change(|index| claim_receipt(index, job, expected))
    }

    pub(crate) fn claim_with_wait(
        &self,
        job: &JobId,
        expected: &Receipt,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        require_running(stopped)?;
        let directory = Directory::open(&self.0, false)?;
        let _lock = wait_claim_lock(directory.open_lock(LOCK)?, stopped)?;
        Self::change_locked(&directory, |index| {
            require_running(stopped)?;
            claim_receipt(index, job, expected)
        })
    }

    pub(crate) fn require_claimed(&self, job: &JobId, expected: &Receipt) -> io::Result<()> {
        let directory = Directory::open(&self.0, false)?;
        let mut index = read_index(&directory)?;
        let mut claimed = expected.clone();
        claimed.used = true;
        if index.remove(job).as_ref() != Some(&claimed) {
            return Err(needs_review());
        }
        Ok(())
    }

    fn snapshot(&self) -> io::Result<Index> {
        read_index(&Directory::open(&self.0, false)?)
    }

    fn change<T>(&self, change: impl FnOnce(&mut Index) -> io::Result<T>) -> io::Result<T> {
        let directory = Directory::open(&self.0, false)?;
        let file = directory.open_lock(LOCK)?;
        file.try_lock().map_err(io::Error::other)?;
        let _lock = Lock(file);
        Self::change_locked(&directory, change)
    }

    fn change_locked<T>(
        directory: &Directory,
        change: impl FnOnce(&mut Index) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut index = read_index(directory)?;
        let result = change(&mut index)?;
        let bytes = serde_json::to_vec(&index)?;
        if bytes.len() as u64 > MAX_BYTES
            || index.len() > kortty_jobs::MAX_JOBS
            || !index.values().all(Receipt::valid)
        {
            return Err(io::Error::other(
                "The scheduled SFTP review store exceeds its limits.",
            ));
        }
        directory.replace_file(FILE, &bytes)?;
        directory.sync()?;
        Ok(result)
    }
}

fn wait_claim_lock(file: File, stopped: &dyn Fn() -> bool) -> io::Result<Lock> {
    let deadline = Instant::now() + CLAIM_WAIT;
    loop {
        require_running(stopped)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another execution is updating SFTP folder approvals.",
            ));
        }
        match file.try_lock() {
            Ok(()) => return Ok(Lock(file)),
            Err(TryLockError::WouldBlock) => std::thread::sleep(remaining.min(CLAIM_POLL)),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn require_running(stopped: &dyn Fn() -> bool) -> io::Result<()> {
    if stopped() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "The SFTP folder copy stopped before claiming its approval.",
        ))
    } else {
        Ok(())
    }
}

fn claim_receipt(index: &mut Index, job: &JobId, expected: &Receipt) -> io::Result<()> {
    let receipt = index
        .get_mut(job)
        .filter(|stored| **stored == *expected && !stored.used);
    receipt.ok_or_else(needs_review)?.used = true;
    Ok(())
}

struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn read_index(directory: &Directory) -> io::Result<Index> {
    let file = match directory.open_private_file(FILE) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Index::new()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::other(
            "The scheduled SFTP review store exceeds its limits.",
        ));
    }
    let index: Index = serde_json::from_slice(&bytes)?;
    if index.len() > kortty_jobs::MAX_JOBS || !index.values().all(Receipt::valid) {
        return Err(io::Error::other(
            "The scheduled SFTP review store is invalid.",
        ));
    }
    Ok(index)
}

fn needs_review() -> io::Error {
    io::Error::other("This scheduled SFTP copy needs a fresh folder review.")
}

#[cfg(test)]
mod creation_scope_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler_window::tests::Profile;
    use std::cell::Cell;
    use std::sync::mpsc;

    #[test]
    fn sibling_sftp_workers_wait_for_the_shared_receipt_lock_and_both_claim() {
        let profile = Profile::temporary();
        let path = profile.file();
        let profile = path.parent().unwrap();
        let store = Store::new(profile);
        let directory = Directory::open(profile, false).unwrap();
        let entries = ["first-copy", "second-copy"].map(|name| {
            let job = JobId::new(name).unwrap();
            let receipt =
                Receipt::for_operation([1; 32], [2; 32], vec!["pinned route".into()]).unwrap();
            store.swap(&job, receipt.clone(), |_| true).unwrap();
            (job, receipt)
        });
        let held = directory.open_lock(LOCK).unwrap();
        held.try_lock().unwrap();
        std::thread::scope(|scope| {
            let (retrying, waiting) = mpsc::channel();
            let workers = entries.each_ref().map(|(job, receipt)| {
                let (retrying, store) = (retrying.clone(), &store);
                scope.spawn(move || {
                    let checks = Cell::new(0);
                    store.claim_with_wait(job, receipt, &|| {
                        checks.set(checks.get() + 1);
                        // The third check is the first retry after lock contention.
                        if checks.get() == 3 {
                            retrying.send(()).unwrap();
                        }
                        false
                    })
                })
            });
            let both_waiting = (0..entries.len()).all(|_| waiting.recv_timeout(CLAIM_WAIT).is_ok());
            drop(held);
            assert!(both_waiting);
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        for (job, receipt) in &entries {
            store.require_claimed(job, receipt).unwrap();
        }
    }

    #[test]
    fn cancelled_sftp_claim_wait_leaves_the_receipt_available() {
        let profile = Profile::temporary();
        let path = profile.file();
        let profile = path.parent().unwrap();
        let store = Store::new(profile);
        let directory = Directory::open(profile, false).unwrap();
        let job = JobId::new("cancelled-copy").unwrap();
        let receipt =
            Receipt::for_operation([1; 32], [2; 32], vec!["pinned route".into()]).unwrap();
        store.swap(&job, receipt.clone(), |_| true).unwrap();
        let held = directory.open_lock(LOCK).unwrap();
        held.try_lock().unwrap();
        assert!(store.swap(&job, receipt.clone(), |_| true).is_err());

        let checks = Cell::new(0);
        let error = store
            .claim_with_wait(&job, &receipt, &|| {
                checks.set(checks.get() + 1);
                checks.get() == 3
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        drop(held);
        assert!(
            store
                .load(&job, Some(&receipt.approval), receipt.scope)
                .is_ok()
        );
        store.claim_with_wait(&job, &receipt, &|| false).unwrap();
        store.require_claimed(&job, &receipt).unwrap();
    }

    #[test]
    fn cascade_worker_requires_the_exact_confirmed_receipt_even_with_the_same_id_and_scope() {
        let profile = Profile::temporary();
        let store = Store::new(profile.file().parent().unwrap());
        let id = JobId::new("copy").unwrap();
        let action = serde_json::from_value(serde_json::json!({"command": "true"})).unwrap();
        let graph = kortty_jobs::JobGraph::new(vec![
            kortty_jobs::Job::new(id.clone(), "Copy", action).unwrap(),
        ])
        .unwrap();
        let request = kortty_jobs::Scheduler::new(graph)
            .start_cascade(&id, SystemTime::now())
            .unwrap()
            .started
            .pop()
            .unwrap();
        let confirmed =
            Receipt::for_operation([1; 32], [2; 32], vec!["pinned route".into()]).unwrap();
        let approval = Some(confirmed.approval.as_str());
        store.swap(&id, confirmed.clone(), |_| true).unwrap();
        let context = RunContext {
            store: store.clone(),
            scope: confirmed.scope,
            run: request.id,
            job: request.job.clone(),
            action: Arc::clone(&request.action),
            started_at: request.started_at,
            trigger: request.trigger,
            cancellation: request.cancellation.clone(),
            cascade_receipt: Some(confirmed.clone()),
            standing: None,
        };
        assert!(context.permits(&request));
        assert!(context.load_receipt(approval).is_ok());

        let replacement = Receipt {
            proof: [3; 32],
            ..confirmed.clone()
        };
        store.swap(&id, replacement, |_| true).unwrap();
        assert!(store.load(&id, approval, confirmed.scope).is_ok());
        assert!(context.load_receipt(approval).is_err());

        store.swap(&id, confirmed.clone(), |_| true).unwrap();
        store.claim(&id, &confirmed).unwrap();
        assert!(context.load_receipt(approval).is_err());
    }

    #[test]
    fn scheduled_sftp_condition_binds_predecessors_and_maximum_success_age() {
        let copy: Copy = serde_json::from_value(serde_json::json!({
            "local": "/fixture/source", "remote": "/fixture/destination", "direction": "upload",
        }))
        .unwrap();
        let connection: ConnectionRef = serde_json::from_value(serde_json::json!({
            "id": 7, "target": {"host": "server.example", "port": 22, "username": "operator"},
        }))
        .unwrap();
        let predecessors = [
            JobId::new("prepare-source").unwrap(),
            JobId::new("verify-source").unwrap(),
        ];
        let age = NonZeroU32::new(15);
        let gate = DependencyMode::Gate;
        let chain = DependencyMode::Chain;
        let historical = copy.scope(&connection, None).unwrap();
        assert_eq!(
            copy.conditioned_scope(&connection, None, gate, &[], None)
                .unwrap(),
            historical
        );
        assert_eq!(
            copy.conditioned_scope(&connection, None, gate, &[], age)
                .unwrap(),
            historical
        );
        let reviewed = copy
            .conditioned_scope(&connection, None, gate, &predecessors[..1], age)
            .unwrap();
        assert_ne!(reviewed, historical);
        let prior_gate_scope = serde_json::to_vec(&(
            "kortty-scheduled-sftp-condition-v1",
            historical,
            gate,
            &predecessors[..1],
            age,
        ))
        .unwrap();
        assert_eq!(
            reviewed.as_slice(),
            HashAlgorithm::Sha256
                .digest(&prior_gate_scope)
                .unwrap()
                .as_slice()
        );
        assert_ne!(
            reviewed,
            copy.conditioned_scope(&connection, None, chain, &predecessors[..1], age)
                .unwrap()
        );
        assert!(
            copy.conditioned_scope(&connection, None, chain, &[], age)
                .is_err()
        );
        assert_ne!(
            reviewed,
            copy.conditioned_scope(&connection, None, gate, &predecessors[1..], age)
                .unwrap()
        );
        assert_ne!(
            reviewed,
            copy.conditioned_scope(&connection, None, gate, &predecessors[..1], None)
                .unwrap()
        );
        assert_ne!(
            reviewed,
            copy.conditioned_scope(&connection, None, gate, &predecessors, age)
                .unwrap()
        );
    }

    #[test]
    fn scheduled_sftp_condition_keeps_single_start_and_privileged_limits() {
        let predecessors = [JobId::new("prepare-source").unwrap()];
        let day = "2026-12-02".parse().unwrap();
        let schedule = JobSchedule::default()
            .with_active_dates(Some(day), Some(day))
            .unwrap()
            .with_fixed_times(["01:30".parse().unwrap()]);
        assert!(validate_scheduling(None, DependencyMode::Gate, &predecessors, false).is_ok());
        assert!(
            validate_scheduling(Some(&schedule), DependencyMode::Gate, &predecessors, false)
                .is_ok()
        );
        assert!(validate_scheduling(None, DependencyMode::Gate, &predecessors, true).is_err());
        assert!(validate_scheduling(None, DependencyMode::Gate, &[], true).is_ok());
        assert!(validate_scheduling(None, DependencyMode::Chain, &[], false).is_err());
        assert!(validate_scheduling(None, DependencyMode::Chain, &predecessors, true).is_err());
        assert!(validate_scheduling(None, DependencyMode::Chain, &predecessors, false).is_ok());
        assert!(
            validate_scheduling(Some(&schedule), DependencyMode::Chain, &predecessors, false)
                .is_ok()
        );
        let recurring = schedule.with_interval_minutes(NonZeroU32::new(30));
        for mode in [DependencyMode::Gate, DependencyMode::Chain] {
            assert!(validate_scheduling(Some(&recurring), mode, &predecessors, false).is_err());
        }
    }
}
