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
use crate::scheduler::reserve_run_ids_through;
use crate::{
    DefinitionError, GraphError, JobDefinition, JobGraph, JobId, MAX_DEPENDENCIES, MAX_JOBS, RunId,
    RunOutcome, RunRecord, RunTrigger, Scheduler,
};

const VERSION: u32 = 1;
const MAX_STORED_RUN_ID: u64 = u64::MAX - u32::MAX as u64;
const MAX_JOURNAL_DETAIL_BYTES: usize = 4096;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Maximum number of retained finished records. The oldest appended records
/// are dropped when this limit is reached.
pub const MAX_JOURNAL_RECORDS: usize = 256;
/// Maximum accepted or written size of the scheduler file.
pub const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;

/// Opaque name component for a recording stored beside the scheduler profile.
pub type JournalRecording = [u8; 16];

/// Exclusive owner of one native scheduler profile. Write operations update
/// the in-memory data only after replacing the file successfully.
pub struct SchedulerStore {
    path: PathBuf,
    _lock: ProfileLock,
    data: StoreData,
}

struct ProfileLock(File);

impl Drop for ProfileLock {
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    actions: BTreeMap<JobId, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    journal_details: BTreeMap<RunId, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    journal_recordings: BTreeMap<RunId, JournalRecording>,
}

impl Default for StoreData {
    fn default() -> Self {
        Self {
            version: VERSION,
            definitions: Vec::new(),
            journal: Vec::new(),
            last_runs: BTreeMap::new(),
            high_watermark: 0,
            actions: BTreeMap::new(),
            journal_details: BTreeMap::new(),
            journal_recordings: BTreeMap::new(),
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
        let lock_path = directory.join(lock_name(name));
        require_regular_if_present(&lock_path)?;
        let lock = private_options()
            .read(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(StoreError::InUse),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let lock = ProfileLock(lock);
        let data = read_store(&path)?;
        data.validate()?;
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

    /// Projects the persisted scheduler into portable definitions and actions.
    /// Device-local run history, attachment references, and counters are excluded.
    pub fn portable_backup(bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        let data = decode_store(bytes)?;
        data.validate()?;
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
        let data = decode_store(bytes)?;
        data.validate()?;
        if !data.journal.is_empty()
            || !data.last_runs.is_empty()
            || data.high_watermark != 0
            || !data.journal_details.is_empty()
            || !data.journal_recordings.is_empty()
        {
            return Err(StoreError::InvalidRecord);
        }
        for definition in &data.definitions {
            let action = data
                .actions
                .get(&definition.id)
                .ok_or_else(|| StoreError::MissingAction(definition.id.clone()))?;
            let action = A::deserialize(action)
                .map_err(|_| StoreError::InvalidAction(definition.id.clone()))?;
            if !action_valid(action) {
                return Err(StoreError::InvalidAction(definition.id.clone()));
            }
        }
        Ok(())
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

    /// Optional executor-supplied summary attached to a retained run.
    pub fn journal_detail(&self, id: RunId) -> Option<&str> {
        self.data.journal_details.get(&id).map(String::as_str)
    }

    /// Identifies the recording attached to a retained run.
    pub fn journal_recording(&self, id: RunId) -> Option<JournalRecording> {
        self.data.journal_recordings.get(&id).copied()
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

    /// Refreshes live dependency history after journal deletion or retention.
    /// Commit any outstanding finished records before applying the history.
    pub fn apply_history<A>(&self, scheduler: &mut Scheduler<A>) {
        let last_runs = self
            .data
            .last_runs
            .iter()
            .map(|(id, &time)| (id, SystemTime::from(time)));
        scheduler.restore_history(last_runs, &self.data.journal);
    }

    /// Atomically saves definitions and finished records from a scheduler
    /// operation. Existing identical records are accepted for retry while
    /// they are retained. Executor data is never serialized.
    pub fn commit<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
    ) -> Result<(), StoreError> {
        self.commit_inner(graph, records, None, &[], &[])
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
        self.commit_inner(graph, records, None, details, recordings)
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
        let actions = graph
            .jobs()
            .iter()
            .map(|job| {
                serde_json::to_value(job.action())
                    .map(|action| (job.id().clone(), action))
                    .map_err(|_| StoreError::InvalidAction(job.id().clone()))
            })
            .collect::<Result<_, _>>()?;
        self.commit_inner(graph, records, Some(actions), details, &[])
    }

    fn commit_inner<A>(
        &mut self,
        graph: &JobGraph<A>,
        records: &[RunRecord],
        actions: Option<BTreeMap<JobId, Value>>,
        details: &[(RunId, String)],
        recordings: &[(RunId, JournalRecording)],
    ) -> Result<(), StoreError> {
        if records.len() > MAX_JOURNAL_RECORDS {
            return Err(StoreError::TooManyRecords);
        }
        let mut next = self.data.clone();
        next.definitions = graph.jobs().iter().map(crate::Job::definition).collect();
        if let Some(actions) = actions {
            next.actions = actions;
        } else {
            next.actions.retain(|id, _| graph.job(id).is_some());
        }
        next.last_runs.retain(|id, _| graph.job(id).is_some());
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
            if record.counted_as_run && graph.job(&record.job).is_some() {
                let finished =
                    timestamp(record.finished_at).map_err(|_| StoreError::InvalidRecord)?;
                let last = next.last_runs.entry(record.job.clone()).or_insert(finished);
                *last = (*last).max(finished);
            }
            next.high_watermark = next.high_watermark.max(record.id.value());
            indices.insert(record.id, next.journal.len());
            next.journal.push(record.clone());
        }
        for (id, detail) in details {
            if !indices.contains_key(id) || !valid_detail(detail) {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.journal_details.insert(*id, detail.clone())
                && previous != *detail
            {
                return Err(StoreError::ConflictingRecord(*id));
            }
        }
        for &(id, recording) in recordings {
            if !indices.contains_key(&id) {
                return Err(StoreError::InvalidRecord);
            }
            if let Some(previous) = next.journal_recordings.insert(id, recording)
                && previous != recording
            {
                return Err(StoreError::ConflictingRecord(id));
            }
        }
        let excess = next.journal.len().saturating_sub(MAX_JOURNAL_RECORDS);
        next.journal.drain(..excess);
        next.retain_journal_metadata();
        self.persist(next)
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
        if self.data == next {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&next).map_err(|_| StoreError::InvalidRecord)?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err(StoreError::TooLarge);
        }
        write_atomically(&self.path, &bytes)?;
        self.data = next;
        Ok(())
    }
}

impl StoreData {
    fn retain_journal_metadata(&mut self) {
        if !self.journal_details.is_empty() || !self.journal_recordings.is_empty() {
            let retained: HashSet<_> = self.journal.iter().map(|record| record.id).collect();
            self.journal_details.retain(|id, _| retained.contains(id));
            self.journal_recordings
                .retain(|id, _| retained.contains(id));
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
        let mut known = HashSet::new();
        for record in &self.journal {
            validate_record(record)?;
            if record.id.value() > self.high_watermark || !known.insert(record.id) {
                return Err(StoreError::InvalidRecord);
            }
        }
        if self
            .journal_details
            .iter()
            .any(|(id, detail)| !known.contains(id) || !valid_detail(detail))
            || self.journal_recordings.keys().any(|id| !known.contains(id))
        {
            return Err(StoreError::InvalidRecord);
        }
        Ok(())
    }
}

fn valid_detail(detail: &str) -> bool {
    detail.len() <= MAX_JOURNAL_DETAIL_BYTES
        && !detail.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
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
