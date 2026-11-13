use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::validate_name;
use crate::schedule::timestamp;
use crate::scheduler::{DependencyResult, reserve_run_ids_through};
use crate::{
    DefinitionError, GraphError, JobDefinition, JobGraph, JobId, MAX_DEPENDENCIES, MAX_JOBS,
    ProfileRef, RunId, RunOutcome, RunRecord, RunTrigger, Scheduler,
};

const VERSION: u32 = 1;
const MAX_STORED_RUN_ID: u64 = u64::MAX - u32::MAX as u64;
const MAX_JOURNAL_DETAIL_BYTES: usize = 4096;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Maximum number of retained finished records. Older records also expire
/// when newer records would exceed the scheduler file's byte limit.
pub const MAX_JOURNAL_RECORDS: usize = 256;
/// Maximum accepted or written size of the scheduler file.
pub const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;

/// Opaque name component for a recording stored beside the scheduler profile.
pub type JournalRecording = [u8; 16];

/// Maximum number of target journals attached to a single scheduler run.
pub const MAX_SESSION_JOURNALS_PER_RUN: usize = MAX_SWARM_TARGETS;

/// Maximum selected targets in one scheduled Swarm.
pub const MAX_SWARM_TARGETS: usize = 32;
pub const MAX_SWARM_QUESTION_BYTES: usize = 4096;
pub const MAX_SWARM_TARGET_LABEL_BYTES: usize = 512;
pub const MAX_SWARM_TARGET_DETAIL_BYTES: usize = 2048;
pub const MAX_SWARM_COMPARISON_BYTES: usize = 8 * 1024;
const MAX_PENDING_SWARM_CHAT_SEEDS: usize = 32;
const MAX_PENDING_SWARM_CHAT_BYTES: usize = 2 * 1024 * 1024;

/// Masked advisory results from one finished Swarm firing, in reviewed target order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmReport {
    pub question: String,
    pub targets: Vec<SwarmTargetReport>,
    /// A masked model comparison of these targets' results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<String>,
}

/// One target's outcome and masked, synthesized detail.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmTargetReport {
    pub target: u64,
    pub label: String,
    pub outcome: RunOutcome,
    pub detail: String,
}

/// A profile-local saved conversation identity for a committed Swarm report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmChatOrigin {
    pub nonce: String,
    pub profile: ProfileRef,
}

impl SwarmChatOrigin {
    fn valid(&self) -> bool {
        self.profile.is_nonzero() && valid_nonce(&self.nonce)
    }
}

/// A masked conversation awaiting delivery to the private saved-chat store.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmChatSeed {
    pub run: RunId,
    pub origin: SwarmChatOrigin,
    pub report: SwarmReport,
}

/// Local journal metadata committed with finished runs.
#[derive(Default)]
pub struct FinishedRunMetadata<'a> {
    pub details: &'a [(RunId, String)],
    pub recordings: &'a [(RunId, JournalRecording)],
    pub session_journals: &'a [(RunId, &'a [String])],
    pub swarm_reports: &'a [(RunId, SwarmReport)],
    pub swarm_chat_origins: &'a [(RunId, SwarmChatOrigin)],
}

impl SwarmReport {
    pub fn is_valid_for(&self, record: &RunRecord) -> bool {
        record.counted_as_run
            && record.blockers.is_empty()
            && matches!(record.trigger, RunTrigger::Manual | RunTrigger::Scheduled)
            && self.outcome() == Some(record.outcome)
    }

    fn outcome(&self) -> Option<RunOutcome> {
        if !(1..=MAX_SWARM_TARGETS).contains(&self.targets.len())
            || !valid_report_text(&self.question, MAX_SWARM_QUESTION_BYTES)
            || !self
                .comparison
                .as_deref()
                .is_none_or(valid_swarm_comparison)
        {
            return None;
        }

        let mut ids = HashSet::new();
        let mut aggregate = RunOutcome::Success;
        for target in &self.targets {
            if target.target == 0
                || !ids.insert(target.target)
                || !valid_report_text(&target.label, MAX_SWARM_TARGET_LABEL_BYTES)
                || !valid_report_text(&target.detail, MAX_SWARM_TARGET_DETAIL_BYTES)
            {
                return None;
            }
            match target.outcome {
                RunOutcome::Cancelled => return None,
                failed @ RunOutcome::Failed { .. }
                    if !matches!(aggregate, RunOutcome::Failed { .. }) =>
                {
                    aggregate = failed;
                }
                RunOutcome::Blocked if aggregate == RunOutcome::Success => {
                    aggregate = RunOutcome::Blocked;
                }
                _ => {}
            }
        }
        Some(aggregate)
    }
}

/// Sanitized delivery results attached to a finished scheduler run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WebhookOutcome {
    Delivered,
    PolicyDenied,
    TargetDisabled,
    MissingCredential,
    InvalidDestination,
    Rejected,
    Unavailable,
    Revoked,
    QueueFull,
}

pub const MAX_WEBHOOKS_PER_RUN: usize = 8;

/// Exclusive owner of one native scheduler profile. Write operations update
/// the in-memory data only after replacing the file successfully.
pub struct SchedulerStore {
    path: PathBuf,
    _lock: StoreLock,
    data: StoreData,
}

/// An additive update for a scheduler profile, ready for the caller to publish.
#[derive(Default)]
pub struct PreparedJobAdditions {
    pub bytes: Option<Vec<u8>>,
    pub added: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub missing_dependencies: usize,
    pub full: usize,
}

pub struct StoreLock(File);

impl StoreLock {
    /// Takes the same lock as a live scheduler without opening its database.
    pub fn acquire(path: &Path) -> Result<Self, StoreError> {
        let name = path.file_name().ok_or(StoreError::UnsafePath)?;
        let lock_path = path.with_file_name(lock_name(name));
        require_regular_if_present(&lock_path)?;
        let mut options = private_options();
        options.read(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(OPEN_REPARSE_POINT);
        }
        let lock = options.open(lock_path)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            const REPARSE_POINT: u32 = 0x400;
            if lock.metadata()?.file_attributes() & REPARSE_POINT != 0 {
                return Err(StoreError::UnsafePath);
            }
        }
        match lock.try_lock() {
            Ok(()) => Ok(Self(lock)),
            Err(TryLockError::WouldBlock) => Err(StoreError::InUse),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        // A forked PTY child can still hold a copy of the descriptor until
        // exec. Release the lock explicitly before closing our descriptor.
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct StoreData {
    version: u32,
    definitions: Vec<JobDefinition>,
    journal: Vec<RunRecord>,
    last_runs: BTreeMap<JobId, Timestamp>,
    high_watermark: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dependency_results: Option<BTreeMap<JobId, DependencyResult>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    actions: BTreeMap<JobId, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    journal_details: BTreeMap<RunId, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    journal_recordings: BTreeMap<RunId, JournalRecording>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    session_journals: BTreeMap<RunId, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    swarm_reports: BTreeMap<RunId, SwarmReport>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    swarm_chat_origins: BTreeMap<RunId, SwarmChatOrigin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    swarm_chat_seeds: BTreeMap<RunId, SwarmChatSeed>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    webhook_results: BTreeMap<RunId, BTreeMap<String, WebhookOutcome>>,
}

impl Default for StoreData {
    fn default() -> Self {
        Self {
            version: VERSION,
            definitions: Vec::new(),
            journal: Vec::new(),
            last_runs: BTreeMap::new(),
            high_watermark: 0,
            dependency_results: None,
            actions: BTreeMap::new(),
            journal_details: BTreeMap::new(),
            journal_recordings: BTreeMap::new(),
            session_journals: BTreeMap::new(),
            swarm_reports: BTreeMap::new(),
            swarm_chat_origins: BTreeMap::new(),
            swarm_chat_seeds: BTreeMap::new(),
            webhook_results: BTreeMap::new(),
        }
    }
}

impl SchedulerStore {
    /// Opens a native JSON profile, creating parent directories if needed.
    /// An absent file starts empty. Existing invalid files stay in place and
    /// cannot be overwritten through a store that failed to open.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let name = path.file_name().ok_or(StoreError::UnsafePath)?;
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        create_directories(directory)?;
        let directory = fs::canonicalize(directory)?;
        let path = directory.join(name);
        let lock = StoreLock::acquire(&path)?;
        let mut data = read_store(&path)?;
        data.validate()?;
        data.migrate_dependency_results();
        reserve_run_ids_through(data.high_watermark);
        Ok(Self {
            path,
            _lock: lock,
            data,
        })
    }

    /// Saved definitions, in the order used by dependency diagrams.
    pub fn definitions(&self) -> &[JobDefinition] {
        &self.data.definitions
    }

    /// Prepares an additive merge of an independently validated graph of disabled jobs.
    /// A local job can satisfy an incoming dependency only when its definition and
    /// action exactly match the incoming job. Historical IDs remain reserved.
    /// The caller must publish against the same local bytes under its profile lock.
    pub fn prepare_disabled_additions<A: Serialize>(
        current: Option<&[u8]>,
        incoming: &JobGraph<A>,
    ) -> Result<PreparedJobAdditions, StoreError> {
        let mut data = current.map(decode_store).transpose()?.unwrap_or_default();
        data.validate()?;
        if incoming.jobs().iter().any(crate::Job::enabled) {
            return Err(StoreError::InvalidRecord);
        }

        let mut result = PreparedJobAdditions::default();
        let local: HashMap<_, _> = data
            .definitions
            .iter()
            .enumerate()
            .map(|(index, definition)| (definition.id.clone(), index))
            .collect();
        let historical: HashSet<_> = data.journal.iter().map(|record| &record.job).collect();
        let mut available = HashSet::new();
        let mut edges: usize = data
            .definitions
            .iter()
            .map(|job| job.dependencies.len())
            .sum();
        let mut size = json_size(&data)?;

        for &index in &incoming.topological {
            let job = &incoming.jobs()[index];
            if !job.dependencies().iter().all(|id| available.contains(id)) {
                result.missing_dependencies += 1;
                continue;
            }

            let definition = job.definition();
            let id = &definition.id;
            let action = serde_json::to_value(job.action())
                .map_err(|_| StoreError::InvalidAction(id.clone()))?;
            if let Some(&index) = local.get(id) {
                if data.definitions[index] == definition && data.actions.get(id) == Some(&action) {
                    result.unchanged += 1;
                    available.insert(job.id());
                } else {
                    result.conflicts += 1;
                }
                continue;
            }
            if historical.contains(id) {
                result.conflicts += 1;
                continue;
            }

            let definition_bytes =
                json_size(&definition)? + usize::from(!data.definitions.is_empty());
            let action_bytes = json_size(id)? + 1 + json_size(&action)?;
            // The actions field is omitted when empty. Later entries only add a comma.
            let action_overhead = if data.actions.is_empty() {
                br#","actions":{}"#.len()
            } else {
                1
            };
            let next_size = size
                .saturating_add(definition_bytes)
                .saturating_add(action_bytes)
                .saturating_add(action_overhead);
            let next_edges = edges + definition.dependencies.len();
            if data.definitions.len() == MAX_JOBS
                || next_edges > MAX_DEPENDENCIES
                || next_size as u64 > MAX_STORE_BYTES
            {
                result.full += 1;
                continue;
            }

            data.actions.insert(id.clone(), action);
            data.definitions.push(definition);
            available.insert(job.id());
            size = next_size;
            edges = next_edges;
            result.added += 1;
        }
        if result.added > 0 {
            data.validate()?;
            let bytes = serde_json::to_vec(&data).map_err(StoreError::invalid_json)?;
            debug_assert_eq!(bytes.len(), size);
            if bytes.len() as u64 > MAX_STORE_BYTES {
                return Err(StoreError::TooLarge);
            }
            result.bytes = Some(bytes);
        }
        Ok(result)
    }

    /// Projects the persisted scheduler into portable definitions and actions.
    /// Device-local run history, attachment references, and counters are excluded.
    pub fn portable_backup(bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        Self::portable_backup_with(bytes, |_| {})
    }

    /// Projects portable actions after removing caller-owned device authorizations.
    pub fn portable_backup_with(
        bytes: &[u8],
        mut project: impl FnMut(&mut Value),
    ) -> Result<Vec<u8>, StoreError> {
        let mut data = decode_store(bytes)?;
        data.validate()?;
        data.actions.values_mut().for_each(&mut project);
        let portable = StoreData {
            definitions: data.definitions,
            actions: data.actions,
            ..StoreData::default()
        };
        serde_json::to_vec(&portable).map_err(StoreError::invalid_json)
    }

    /// Validates portable definitions, the dependency graph, and each typed action.
    pub fn validate_backup<A: DeserializeOwned>(
        bytes: &[u8],
        action_valid: impl Fn(A) -> bool,
    ) -> Result<(), StoreError> {
        let data = decode_backup(bytes)?;
        for definition in &data.definitions {
            if !action_valid(data.backup_action(&definition.id)?) {
                return Err(StoreError::InvalidAction(definition.id.clone()));
            }
        }
        Ok(())
    }

    /// Serializes a validated graph and its actions without device-local history.
    pub fn encode_backup<A: Serialize>(graph: &JobGraph<A>) -> Result<Vec<u8>, StoreError> {
        let data = StoreData {
            definitions: graph.jobs().iter().map(crate::Job::definition).collect(),
            actions: encode_actions(graph)?,
            ..StoreData::default()
        };
        let bytes = serde_json::to_vec(&data).map_err(StoreError::invalid_json)?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err(StoreError::TooLarge);
        }
        Ok(bytes)
    }

    /// Reads a portable graph with caller-defined actions and no run history.
    pub fn decode_backup<A: DeserializeOwned>(bytes: &[u8]) -> Result<JobGraph<A>, StoreError> {
        let data = decode_backup(bytes)?;
        let jobs = data
            .definitions
            .iter()
            .map(|definition| {
                let action = data.backup_action(&definition.id)?;
                Ok(definition.clone().with_action(action)?)
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(JobGraph::new(jobs)?)
    }

    /// Retained finished records ordered by most recent start. Equal starts
    /// retain append order. A deleted job's name remains in its entries.
    pub fn journal(&self) -> Vec<&RunRecord> {
        let mut journal: Vec<_> = self.data.journal.iter().collect();
        journal.sort_by_key(|record| Reverse(record.started_at));
        journal
    }

    /// Looks up a retained run without allocating the sorted journal view.
    pub fn journal_record(&self, id: RunId) -> Option<&RunRecord> {
        self.data.journal.iter().find(|record| record.id == id)
    }

    /// The most recent committed completion, with later appends winning ties.
    pub fn latest_finished(&self, job: &JobId) -> Option<&RunRecord> {
        self.data
            .journal
            .iter()
            .filter(|record| &record.job == job)
            .max_by_key(|record| record.finished_at)
    }

    /// Optional executor-supplied summary attached to a retained run.
    pub fn journal_detail(&self, id: RunId) -> Option<&str> {
        self.data.journal_details.get(&id).map(String::as_str)
    }

    /// Identifies the recording attached to a retained run.
    pub fn journal_recording(&self, id: RunId) -> Option<JournalRecording> {
        self.data.journal_recordings.get(&id).copied()
    }

    /// Identifies the native per-target Session Journals attached to a retained run.
    pub fn session_journals(&self, id: RunId) -> Option<&[String]> {
        self.data.session_journals.get(&id).map(Vec::as_slice)
    }

    /// Returns the native advisory report for a retained Swarm firing.
    pub fn journal_swarm_report(&self, id: RunId) -> Option<&SwarmReport> {
        self.data.swarm_reports.get(&id)
    }

    pub fn journal_swarm_chat_origin(&self, id: RunId) -> Option<&SwarmChatOrigin> {
        self.data.swarm_chat_origins.get(&id)
    }

    pub fn pending_swarm_chat_seed(&self, id: RunId) -> Option<&SwarmChatSeed> {
        self.data.swarm_chat_seeds.get(&id)
    }

    pub fn next_swarm_chat_seed(
        &self,
        ready: impl Fn(&SwarmChatSeed) -> bool,
    ) -> Option<&SwarmChatSeed> {
        self.data.swarm_chat_seeds.values().find(|seed| ready(seed))
    }

    /// Removes only the exact seed that the saved-chat store has accepted.
    pub fn acknowledge_swarm_chat_seed(&mut self, nonce: &str) -> Result<bool, StoreError> {
        let Some(id) = self
            .data
            .swarm_chat_seeds
            .values()
            .find(|seed| seed.origin.nonce == nonce)
            .map(|seed| seed.run)
        else {
            return Ok(false);
        };
        let mut next = self.data.clone();
        next.swarm_chat_seeds.remove(&id);
        self.persist(next)?;
        Ok(true)
    }

    pub fn profile_root(&self) -> &Path {
        self.path
            .parent()
            .expect("scheduler store has a canonical parent")
    }

    pub fn webhook_results(&self, id: RunId) -> Option<&BTreeMap<String, WebhookOutcome>> {
        self.data.webhook_results.get(&id)
    }

    /// Adds delivery results for retained records, without changing the runs.
    pub fn record_webhook_results(
        &mut self,
        results: &BTreeMap<RunId, BTreeMap<String, WebhookOutcome>>,
    ) -> Result<(), StoreError> {
        let mut next = self.data.clone();
        for (id, additions) in results {
            if self.journal_record(*id).is_none() {
                continue;
            }
            let entries = next.webhook_results.entry(*id).or_default();
            for (target, outcome) in additions {
                if let Some(previous) = entries.insert(target.clone(), *outcome)
                    && previous != *outcome
                {
                    return Err(StoreError::ConflictingRecord(*id));
                }
            }
            if !valid_webhook_results(entries) {
                return Err(StoreError::InvalidRecord);
            }
        }
        self.persist(next)
    }

    /// Lists retained attachments for file cleanup after a journal update.
    pub fn journal_recordings(&self) -> impl Iterator<Item = (RunId, JournalRecording)> + '_ {
        self.data
            .journal_recordings
            .iter()
            .map(|(&id, &name)| (id, name))
    }

    /// Builds a scheduler using execution data supplied by the caller.
    /// Call `initialize_schedules` with the host clock and time zone next.
    pub fn restore<A>(
        &self,
        mut resolve: impl FnMut(&JobDefinition) -> Option<A>,
    ) -> Result<Scheduler<A>, StoreError> {
        self.restore_resolving(|definition| {
            resolve(definition).ok_or_else(|| StoreError::MissingAction(definition.id.clone()))
        })
    }

    /// Restores definitions and their atomically saved, caller-defined action metadata.
    pub fn restore_with_actions<A: DeserializeOwned>(&self) -> Result<Scheduler<A>, StoreError> {
        self.restore_resolving(|definition| {
            let data = self
                .data
                .actions
                .get(&definition.id)
                .ok_or_else(|| StoreError::MissingAction(definition.id.clone()))?;
            A::deserialize(data).map_err(|_| StoreError::InvalidAction(definition.id.clone()))
        })
    }

    fn restore_resolving<A>(
        &self,
        mut resolve: impl FnMut(&JobDefinition) -> Result<A, StoreError>,
    ) -> Result<Scheduler<A>, StoreError> {
        let mut jobs = Vec::with_capacity(self.data.definitions.len());
        for definition in &self.data.definitions {
            jobs.push(definition.clone().with_action(resolve(definition)?)?);
        }
        let mut scheduler = Scheduler::new(JobGraph::new(jobs)?);
        self.apply_history(&mut scheduler);
        Ok(scheduler)
    }

    /// Refreshes live dependency results and retained journal history.
    /// Commit any outstanding finished records before applying the history.
    pub fn apply_history<A>(&self, scheduler: &mut Scheduler<A>) {
        let last_runs = self
            .data
            .last_runs
            .iter()
            .map(|(id, &time)| (id, SystemTime::from(time)));
        let dependency_results = self
            .data
            .dependency_results
            .iter()
            .flatten()
            .map(|(id, &result)| (id, result));
        scheduler.restore_history(last_runs, dependency_results, &self.data.journal);
    }

    /// Atomically saves definitions and finished records from a scheduler
    /// operation. Existing identical records are accepted for retry while
    /// they are retained. Executor data is never serialized.
    pub fn commit<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
    ) -> Result<(), StoreError> {
        self.commit_inner(graph, records, None, FinishedRunMetadata::default())
    }

    /// Commits runtime updates and optional summaries while preserving the
    /// saved action metadata for each surviving job.
    pub fn commit_records<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        details: &[(RunId, String)],
    ) -> Result<(), StoreError> {
        self.commit_records_with_recordings(graph, records, details, &[])
    }

    /// Commits finished runs and references to complete recording files together.
    pub fn commit_records_with_recordings<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        details: &[(RunId, String)],
        recordings: &[(RunId, JournalRecording)],
    ) -> Result<(), StoreError> {
        self.commit_records_with_swarm_reports(graph, records, details, recordings, &[])
    }

    /// Commits finished runs, summaries, recording references and masked Swarm reports
    /// together. Reports may only accompany their corresponding new completion.
    /// Retrying the same completion with its identical report is supported.
    pub fn commit_records_with_swarm_reports<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        details: &[(RunId, String)],
        recordings: &[(RunId, JournalRecording)],
        swarm_reports: &[(RunId, SwarmReport)],
    ) -> Result<(), StoreError> {
        self.commit_records_with_swarm_chat_origins(
            graph,
            records,
            details,
            recordings,
            swarm_reports,
            &[],
        )
    }

    /// Commits Swarm chat origins alongside their first completed reports.
    /// When the delivery queue is full, the retained report can still be opened by origin.
    pub fn commit_records_with_swarm_chat_origins<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        details: &[(RunId, String)],
        recordings: &[(RunId, JournalRecording)],
        swarm_reports: &[(RunId, SwarmReport)],
        chat_origins: &[(RunId, SwarmChatOrigin)],
    ) -> Result<(), StoreError> {
        self.commit_finished_runs(
            graph,
            records,
            FinishedRunMetadata {
                details,
                recordings,
                swarm_reports,
                swarm_chat_origins: chat_origins,
                ..FinishedRunMetadata::default()
            },
        )
    }

    /// Commits completed runs with their optional native metadata in one replacement.
    pub fn commit_finished_runs<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        metadata: FinishedRunMetadata<'_>,
    ) -> Result<(), StoreError> {
        self.commit_inner(graph, records, None, metadata)
    }

    /// Saves serializable action metadata, finished records and optional
    /// summaries in one replacement. Callers should store only non-secret
    /// action metadata and synthesized summaries, never credentials or output.
    pub fn commit_with_actions<A: Serialize>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        details: &[(RunId, String)],
    ) -> Result<(), StoreError> {
        let actions = encode_actions(graph)?;
        self.commit_inner(
            graph,
            records,
            Some(actions),
            FinishedRunMetadata {
                details,
                ..FinishedRunMetadata::default()
            },
        )
    }

    fn commit_inner<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        actions: Option<BTreeMap<JobId, Value>>,
        metadata: FinishedRunMetadata<'_>,
    ) -> Result<(), StoreError> {
        if records.len() > MAX_JOURNAL_RECORDS {
            return Err(StoreError::TooManyRecords);
        }
        if metadata.swarm_reports.len() > records.len()
            || metadata.swarm_chat_origins.len() > metadata.swarm_reports.len()
            || metadata.session_journals.len() > records.len()
        {
            return Err(StoreError::InvalidRecord);
        }
        let mut next = self.data.clone();
        next.definitions = graph.jobs().iter().map(crate::Job::definition).collect();
        if let Some(actions) = actions {
            next.actions = actions;
        } else {
            next.actions.retain(|id, _| graph.job(id).is_some());
        }
        next.last_runs.retain(|id, _| graph.job(id).is_some());
        let dependency_results = next.dependency_results.get_or_insert_with(BTreeMap::new);
        dependency_results.retain(|id, _| graph.job(id).is_some());
        let mut indices: HashMap<_, _> = next
            .journal
            .iter()
            .enumerate()
            .map(|(index, record)| (record.id, index))
            .collect();
        for record in records {
            validate_record(record)?;
            if let Some(&index) = indices.get(&record.id) {
                if &next.journal[index] != record {
                    return Err(StoreError::ConflictingRecord(record.id));
                }
                continue;
            }
            if let Some((job, result)) = dependency_results
                .iter()
                .find(|(_, result)| result.id == record.id)
                && (job != &record.job || *result != DependencyResult::from(record))
            {
                return Err(StoreError::ConflictingRecord(record.id));
            }
            if graph.job(&record.job).is_some() {
                if record.counted_as_run {
                    let finished =
                        timestamp(record.finished_at).map_err(|_| StoreError::InvalidRecord)?;
                    let last = next.last_runs.entry(record.job.clone()).or_insert(finished);
                    *last = (*last).max(finished);
                }
                remember_dependency_result(
                    dependency_results,
                    record,
                    next.last_runs.get(&record.job),
                );
            }
            next.high_watermark = next.high_watermark.max(record.id.value());
            indices.insert(record.id, next.journal.len());
            next.journal.push(record.clone());
        }
        for (id, detail) in metadata.details {
            if !indices.contains_key(id) || !valid_detail(detail) {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.journal_details.insert(*id, detail.clone())
                && previous != *detail
            {
                return Err(StoreError::ConflictingRecord(*id));
            }
        }
        for &(id, recording) in metadata.recordings {
            if !indices.contains_key(&id) {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.journal_recordings.insert(id, recording)
                && previous != recording
            {
                return Err(StoreError::ConflictingRecord(id));
            }
        }
        self.stage_session_journals(&mut next, records, metadata.session_journals)?;
        for (id, report) in metadata.swarm_reports {
            let record = records
                .iter()
                .find(|record| record.id == *id)
                .ok_or(StoreError::InvalidRecord)?;
            if graph.job(&record.job).is_none() || !report.is_valid_for(record) {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.swarm_reports.get(id) {
                if previous != report {
                    return Err(StoreError::ConflictingRecord(*id));
                }
            } else if self.journal_record(*id).is_some() {
                return Err(StoreError::ConflictingRecord(*id));
            } else {
                next.swarm_reports.insert(*id, report.clone());
            }
        }
        self.stage_swarm_chat_origins(&mut next, &metadata)?;
        let appended = next.journal.len() - self.data.journal.len();
        let excess = next.journal.len().saturating_sub(MAX_JOURNAL_RECORDS);
        next.journal.drain(..excess);
        next.retain_journal_metadata();
        if !metadata.swarm_chat_origins.is_empty() {
            next.validate_swarm_chats()?;
        }
        let evictable = if appended == 0 {
            0
        } else {
            records
                .iter()
                .filter_map(|record| indices.get(&record.id))
                .min()
                .map_or(0, |first| first.saturating_sub(excess))
        };
        self.persist_with_journal_retention(next, evictable)
    }

    fn stage_session_journals(
        &self,
        next: &mut StoreData,
        records: &[RunRecord],
        journals: &[(RunId, &[String])],
    ) -> Result<(), StoreError> {
        if journals.is_empty() {
            return Ok(());
        }
        let mut linked: HashSet<_> = self
            .data
            .session_journals
            .values()
            .flatten()
            .map(|id| id.to_ascii_lowercase())
            .collect();
        let mut runs = HashSet::new();
        for &(run, ids) in journals {
            if !valid_session_journal_ids(ids)
                || !runs.insert(run)
                || !records.iter().any(|record| {
                    record.id == run && record.counted_as_run && record.blockers.is_empty()
                })
            {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.session_journals.get(&run) {
                if previous.as_slice() == ids {
                    continue;
                }
                return Err(StoreError::ConflictingRecord(run));
            }
            if self.journal_record(run).is_some()
                || !ids.iter().all(|id| linked.insert(id.to_ascii_lowercase()))
            {
                return Err(StoreError::ConflictingRecord(run));
            }
            next.session_journals.insert(run, ids.to_vec());
        }
        Ok(())
    }

    fn stage_swarm_chat_origins(
        &self,
        next: &mut StoreData,
        metadata: &FinishedRunMetadata<'_>,
    ) -> Result<(), StoreError> {
        if metadata.swarm_chat_origins.is_empty() {
            return Ok(());
        }
        let mut ids = HashSet::new();
        let mut pending_bytes = pending_seed_bytes(next.swarm_chat_seeds.values())?;
        for (id, origin) in metadata.swarm_chat_origins {
            let report = metadata
                .swarm_reports
                .iter()
                .find_map(|(run, report)| (run == id).then_some(report))
                .ok_or(StoreError::InvalidRecord)?;
            if !origin.valid() || !ids.insert(*id) {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.swarm_chat_origins.get(id) {
                if previous != origin {
                    return Err(StoreError::ConflictingRecord(*id));
                }
                continue;
            }
            if self.journal_record(*id).is_some() || next.swarm_chat_seeds.contains_key(id) {
                return Err(StoreError::ConflictingRecord(*id));
            }
            next.swarm_chat_origins.insert(*id, origin.clone());
            if next.swarm_chat_seeds.len() < MAX_PENDING_SWARM_CHAT_SEEDS {
                let seed = SwarmChatSeed {
                    run: *id,
                    origin: origin.clone(),
                    report: report.clone(),
                };
                let size = seed_bytes(&seed)?;
                if pending_bytes + size <= MAX_PENDING_SWARM_CHAT_BYTES {
                    next.swarm_chat_seeds.insert(*id, seed);
                    pending_bytes += size;
                }
            }
        }
        Ok(())
    }

    /// Removes selected journal entries without erasing last-run timestamps.
    pub fn delete_records(&mut self, ids: &[RunId]) -> Result<usize, StoreError> {
        self.retain(|record| !ids.contains(&record.id))
    }

    /// Removes entries finished strictly before the cutoff.
    pub fn prune_before(&mut self, cutoff: SystemTime) -> Result<usize, StoreError> {
        self.retain(|record| record.finished_at >= cutoff)
    }

    fn retain(&mut self, keep: impl Fn(&RunRecord) -> bool) -> Result<usize, StoreError> {
        let mut next = self.data.clone();
        next.journal.retain(keep);
        next.retain_journal_metadata();
        let removed = self.data.journal.len() - next.journal.len();
        self.persist(next)?;
        Ok(removed)
    }

    fn persist(&mut self, next: StoreData) -> Result<(), StoreError> {
        self.persist_with_journal_retention(next, 0)
    }

    fn persist_with_journal_retention(
        &mut self,
        mut next: StoreData,
        evictable: usize,
    ) -> Result<(), StoreError> {
        if next.high_watermark == 0 {
            // Keep definition-only profiles byte-compatible with portable backups.
            next.dependency_results = None;
        }
        if self.data == next {
            return Ok(());
        }
        let mut bytes = serde_json::to_vec(&next).map_err(|_| StoreError::InvalidRecord)?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            let pending = next.swarm_chat_seeds.len();
            // Preserve existing delivery obligations and the newly committed report.
            next.swarm_chat_seeds
                .retain(|id, _| self.data.swarm_chat_seeds.contains_key(id));
            if pending != next.swarm_chat_seeds.len() {
                bytes = serde_json::to_vec(&next).map_err(|_| StoreError::InvalidRecord)?;
            }
        }
        if bytes.len() as u64 > MAX_STORE_BYTES {
            (next, bytes) = fit_journal(next, evictable)?;
        }
        write_atomically(&self.path, &bytes)?;
        self.data = next;
        Ok(())
    }
}

/// Retain the longest suffix that fits without dropping a record from the
/// current commit. Pending chat delivery stays independent of journal retention.
fn fit_journal(data: StoreData, evictable: usize) -> Result<(StoreData, Vec<u8>), StoreError> {
    let mut lower = 1;
    let mut upper = evictable;
    let mut best = None;
    while lower <= upper {
        let remove = lower + (upper - lower) / 2;
        let mut candidate = data.clone();
        candidate.journal.drain(..remove);
        candidate.retain_journal_metadata();
        let bytes = serde_json::to_vec(&candidate).map_err(|_| StoreError::InvalidRecord)?;
        if bytes.len() as u64 <= MAX_STORE_BYTES {
            best = Some((candidate, bytes));
            upper = remove - 1;
        } else {
            lower = remove + 1;
        }
    }
    best.ok_or(StoreError::TooLarge)
}

impl StoreData {
    fn backup_action<A: DeserializeOwned>(&self, id: &JobId) -> Result<A, StoreError> {
        let action = self
            .actions
            .get(id)
            .ok_or_else(|| StoreError::MissingAction(id.clone()))?;
        A::deserialize(action).map_err(|_| StoreError::InvalidAction(id.clone()))
    }

    fn migrate_dependency_results(&mut self) {
        if self.high_watermark == 0 || self.dependency_results.is_some() {
            return;
        }
        let jobs: HashSet<_> = self.definitions.iter().map(|job| &job.id).collect();
        let mut results = BTreeMap::new();
        for record in &self.journal {
            let last_run = self.last_runs.get(&record.job);
            // Older profiles cannot recover an outcome for a removed last run.
            if jobs.contains(&record.job) && (last_run.is_some() || !record.counted_as_run) {
                remember_dependency_result(&mut results, record, last_run);
            }
        }
        self.dependency_results = Some(results);
    }

    fn retain_journal_metadata(&mut self) {
        if !self.journal_details.is_empty()
            || !self.journal_recordings.is_empty()
            || !self.session_journals.is_empty()
            || !self.swarm_reports.is_empty()
            || !self.swarm_chat_origins.is_empty()
            || !self.webhook_results.is_empty()
        {
            let retained: HashSet<_> = self.journal.iter().map(|record| record.id).collect();
            self.journal_details.retain(|id, _| retained.contains(id));
            self.journal_recordings
                .retain(|id, _| retained.contains(id));
            self.session_journals.retain(|id, _| retained.contains(id));
            self.swarm_reports.retain(|id, _| retained.contains(id));
            self.swarm_chat_origins
                .retain(|id, _| retained.contains(id));
            self.webhook_results.retain(|id, _| retained.contains(id));
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.definitions.len() > MAX_JOBS || self.last_runs.len() > MAX_JOBS {
            return Err(GraphError::TooManyJobs.into());
        }
        if self.journal.len() > MAX_JOURNAL_RECORDS {
            return Err(StoreError::TooManyRecords);
        }
        let mut jobs = Vec::with_capacity(self.definitions.len());
        for definition in &self.definitions {
            validate_name(&definition.name)?;
            jobs.push(definition.clone().with_action(())?);
        }
        let graph = JobGraph::new(jobs)?;
        if self.high_watermark > MAX_STORED_RUN_ID
            || self.last_runs.keys().any(|id| graph.job(id).is_none())
            || self.actions.keys().any(|id| graph.job(id).is_none())
        {
            return Err(StoreError::InvalidRecord);
        }
        let mut known = HashMap::new();
        for record in &self.journal {
            validate_record(record)?;
            if record.id.value() > self.high_watermark || known.insert(record.id, record).is_some()
            {
                return Err(StoreError::InvalidRecord);
            }
        }
        let mut result_ids = HashSet::new();
        if self
            .dependency_results
            .iter()
            .flatten()
            .any(|(job, result)| {
                graph.job(job).is_none()
                    || result.id.value() == 0
                    || result.id.value() > self.high_watermark
                    || !result_ids.insert(result.id)
                    || self
                        .last_runs
                        .get(job)
                        .map_or(result.outcome != RunOutcome::Blocked, |&last| {
                            result.finished_at < SystemTime::from(last)
                        })
                    || known.get(&result.id).is_some_and(|&record| {
                        &record.job != job || DependencyResult::from(record) != *result
                    })
            })
        {
            return Err(StoreError::InvalidRecord);
        }
        let mut linked_journals = HashSet::new();
        if self
            .journal_details
            .iter()
            .any(|(id, detail)| !known.contains_key(id) || !valid_detail(detail))
            || self
                .journal_recordings
                .keys()
                .any(|id| !known.contains_key(id))
            || self.session_journals.iter().any(|(run, ids)| {
                !known
                    .get(run)
                    .is_some_and(|record| record.counted_as_run && record.blockers.is_empty())
                    || !valid_session_journal_ids(ids)
                    || !ids
                        .iter()
                        .all(|id| linked_journals.insert(id.to_ascii_lowercase()))
            })
            || self.swarm_reports.iter().any(|(id, report)| {
                !known
                    .get(id)
                    .is_some_and(|record| report.is_valid_for(record))
            })
            || self
                .webhook_results
                .iter()
                .any(|(id, results)| !known.contains_key(id) || !valid_webhook_results(results))
        {
            return Err(StoreError::InvalidRecord);
        }
        self.validate_swarm_chats()
    }

    fn validate_swarm_chats(&self) -> Result<(), StoreError> {
        if self.swarm_chat_seeds.len() > MAX_PENDING_SWARM_CHAT_SEEDS
            || pending_seed_bytes(self.swarm_chat_seeds.values())? > MAX_PENDING_SWARM_CHAT_BYTES
        {
            return Err(StoreError::InvalidRecord);
        }
        let mut nonces = HashMap::new();
        for (id, origin) in &self.swarm_chat_origins {
            if !self.swarm_reports.contains_key(id)
                || !origin.valid()
                || nonces.insert(origin.nonce.as_str(), *id).is_some()
            {
                return Err(StoreError::InvalidRecord);
            }
        }
        for (id, seed) in &self.swarm_chat_seeds {
            if *id != seed.run
                || id.value() == 0
                || id.value() > self.high_watermark
                || !seed.origin.valid()
                || seed.report.outcome().is_none()
                || (self.journal.iter().any(|record| record.id == *id)
                    && (self.swarm_chat_origins.get(id) != Some(&seed.origin)
                        || self.swarm_reports.get(id) != Some(&seed.report)))
                || nonces
                    .insert(seed.origin.nonce.as_str(), *id)
                    .is_some_and(|previous| previous != *id)
            {
                return Err(StoreError::InvalidRecord);
            }
        }
        Ok(())
    }
}

fn remember_dependency_result(
    results: &mut BTreeMap<JobId, DependencyResult>,
    record: &RunRecord,
    last_run: Option<&Timestamp>,
) {
    let result = DependencyResult::from(record);
    if last_run.is_some_and(|&last| result.finished_at < SystemTime::from(last)) {
        return;
    }
    let previous = results.entry(record.job.clone()).or_insert(result);
    if result.newer_than(*previous) {
        *previous = result;
    }
}

fn pending_seed_bytes<'a>(
    mut seeds: impl Iterator<Item = &'a SwarmChatSeed>,
) -> Result<usize, StoreError> {
    seeds.try_fold(0, |total, seed| Ok(total + seed_bytes(seed)?))
}

fn seed_bytes(seed: &SwarmChatSeed) -> Result<usize, StoreError> {
    serde_json::to_vec(seed)
        // Allow space for the enclosing object's numeric key and separators.
        .map(|bytes| bytes.len() + 32)
        .map_err(|_| StoreError::InvalidRecord)
}

fn valid_webhook_results(results: &BTreeMap<String, WebhookOutcome>) -> bool {
    !results.is_empty()
        && results.len() <= MAX_WEBHOOKS_PER_RUN
        && results.keys().all(|target| valid_nonce(target))
}

fn valid_nonce(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The exact UUID spelling accepted in a native Session Journal directory name.
pub fn valid_session_journal_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Validates the bounded ordered set of target journals for one finished run.
pub fn valid_session_journal_ids(ids: &[String]) -> bool {
    !ids.is_empty()
        && ids.len() <= MAX_SESSION_JOURNALS_PER_RUN
        && ids.iter().enumerate().all(|(index, id)| {
            valid_session_journal_id(id)
                && !ids[..index]
                    .iter()
                    .any(|other| other.eq_ignore_ascii_case(id))
        })
}

fn valid_detail(detail: &str) -> bool {
    detail.len() <= MAX_JOURNAL_DETAIL_BYTES
        && !detail.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

fn valid_report_text(text: &str, limit: usize) -> bool {
    text.len() <= limit
        && !text.trim().is_empty()
        && !text.contains(['\u{2028}', '\u{2029}'])
        && valid_detail(text)
}

/// Checks the storage limits for masked comparison Markdown from one scheduled firing.
pub fn valid_swarm_comparison(markdown: &str) -> bool {
    !markdown.is_empty()
        && markdown.len() <= MAX_SWARM_COMPARISON_BYTES
        && markdown.trim() == markdown
        && markdown.chars().all(|ch| {
            matches!(ch, '\n' | '\t')
                || (!ch.is_control()
                    && !matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}'
                        | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}'))
        })
}

fn validate_record(record: &RunRecord) -> Result<(), StoreError> {
    let cascade_skipped = record.trigger == RunTrigger::Cascade
        && record.outcome == RunOutcome::Blocked
        && !record.blockers.is_empty();
    if record.id.value() == 0
        || record.id.value() > MAX_STORED_RUN_ID
        || record.blockers.len() > MAX_DEPENDENCIES
        || (!record.blockers.is_empty() && record.outcome != RunOutcome::Blocked)
        || record.counted_as_run == cascade_skipped
        || validate_name(&record.job_name).is_err()
        || timestamp(record.started_at).is_err()
        || timestamp(record.finished_at).is_err()
    {
        return Err(StoreError::InvalidRecord);
    }
    Ok(())
}

fn read_store(path: &Path) -> Result<StoreData, StoreError> {
    let Some(metadata) = require_regular_if_present(path)? else {
        return Ok(StoreData::default());
    };
    if metadata.len() > MAX_STORE_BYTES {
        return Err(StoreError::TooLarge);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    decode_store(&bytes)
}

fn decode_store(bytes: &[u8]) -> Result<StoreData, StoreError> {
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(StoreError::TooLarge);
    }
    #[derive(Deserialize)]
    struct Header {
        version: u32,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(StoreError::invalid_json)?;
    if header.version != VERSION {
        return Err(StoreError::Version(header.version));
    }
    serde_json::from_slice(bytes).map_err(StoreError::invalid_json)
}

fn decode_backup(bytes: &[u8]) -> Result<StoreData, StoreError> {
    let data = decode_store(bytes)?;
    data.validate()?;
    if !data.journal.is_empty()
        || !data.last_runs.is_empty()
        || data.high_watermark != 0
        || data
            .dependency_results
            .as_ref()
            .is_some_and(|results| !results.is_empty())
        || !data.journal_details.is_empty()
        || !data.journal_recordings.is_empty()
        || !data.session_journals.is_empty()
        || !data.swarm_reports.is_empty()
        || !data.swarm_chat_origins.is_empty()
        || !data.swarm_chat_seeds.is_empty()
        || !data.webhook_results.is_empty()
    {
        return Err(StoreError::InvalidRecord);
    }
    Ok(data)
}

fn encode_actions<A: Serialize>(graph: &JobGraph<A>) -> Result<BTreeMap<JobId, Value>, StoreError> {
    graph
        .jobs()
        .iter()
        .map(|job| {
            serde_json::to_value(job.action())
                .map(|action| (job.id().clone(), action))
                .map_err(|_| StoreError::InvalidAction(job.id().clone()))
        })
        .collect()
}

fn json_size(value: &impl Serialize) -> Result<usize, StoreError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(StoreError::invalid_json)
}

fn require_regular_if_present(path: &Path) -> Result<Option<fs::Metadata>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(StoreError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn lock_name(file: &std::ffi::OsStr) -> OsString {
    let mut lock = file.to_os_string();
    lock.push(".lock");
    lock
}

fn create_directories(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    require_regular_if_present(path)?;
    let directory = path.parent().ok_or(StoreError::UnsafePath)?;
    #[cfg(unix)]
    let directory_handle = File::open(directory)?;
    loop {
        let index = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".jobs-{}-{index}.tmp", std::process::id()));
        let mut file = match private_options().create_new(true).open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| fs::rename(&temporary, path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        #[cfg(unix)]
        directory_handle.sync_all()?;
        return Ok(());
    }
}

/// A native scheduler profile could not be opened, validated or written.
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    InUse,
    UnsafePath,
    Version(u32),
    InvalidJson { line: usize, column: usize },
    TooLarge,
    TooManyRecords,
    InvalidRecord,
    ConflictingRecord(RunId),
    Definition(DefinitionError),
    Graph(GraphError),
    MissingAction(JobId),
    InvalidAction(JobId),
}

impl StoreError {
    fn invalid_json(error: serde_json::Error) -> Self {
        // Serde errors can quote arbitrary document text. Keep coordinates only.
        Self::InvalidJson {
            line: error.line(),
            column: error.column(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "scheduler file I/O failed: {error}"),
            Self::InUse => formatter.write_str("scheduler profile is already open"),
            Self::UnsafePath => formatter.write_str("scheduler profile must use a regular file"),
            Self::Version(version) => {
                write!(formatter, "unsupported scheduler profile version {version}")
            }
            Self::InvalidJson { line, column } => write!(
                formatter,
                "invalid scheduler JSON at line {line}, column {column}"
            ),
            Self::TooLarge => write!(
                formatter,
                "scheduler profile exceeds {MAX_STORE_BYTES} bytes"
            ),
            Self::TooManyRecords => write!(
                formatter,
                "scheduler journal exceeds {MAX_JOURNAL_RECORDS} entries"
            ),
            Self::InvalidRecord => formatter.write_str("invalid scheduler journal record"),
            Self::ConflictingRecord(id) => write!(
                formatter,
                "scheduler journal contains a different record for run {id}"
            ),
            Self::Definition(error) => error.fmt(formatter),
            Self::Graph(error) => error.fmt(formatter),
            Self::MissingAction(id) => write!(formatter, "no execution data supplied for job {id}"),
            Self::InvalidAction(id) => write!(formatter, "invalid execution data for job {id}"),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Definition(error) => Some(error),
            Self::Graph(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DefinitionError> for StoreError {
    fn from(error: DefinitionError) -> Self {
        Self::Definition(error)
    }
}

impl From<GraphError> for StoreError {
    fn from(error: GraphError) -> Self {
        Self::Graph(error)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn closing_the_store_releases_a_shared_lock_descriptor() {
        struct ProfileDirectory(PathBuf);

        impl Drop for ProfileDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let directory = loop {
            let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kortty-jobs-lock-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => break ProfileDirectory(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scheduler test directory: {error}"),
            }
        };
        let path = directory.0.join("scheduler.json");
        let store = SchedulerStore::open(&path).unwrap();
        // A real duplicate models the descriptor briefly held by a forked child.
        let inherited = store._lock.0.try_clone().unwrap();
        assert!(matches!(
            SchedulerStore::open(&path),
            Err(StoreError::InUse)
        ));

        drop(store);
        let reopened = SchedulerStore::open(&path).unwrap();
        drop(inherited);
        assert!(matches!(
            SchedulerStore::open(&path),
            Err(StoreError::InUse)
        ));
        drop(reopened);
    }
}
