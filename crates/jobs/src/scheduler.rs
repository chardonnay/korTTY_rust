use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::schedule::timestamp;
use crate::{DependencyMode, Job, JobGraph, JobId, ScheduleError, TimeZone};

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// Process-local ticket identifying one execution attempt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct RunId(u64);

impl RunId {
    /// Returns the process-local ticket for passing through a native UI bridge.
    pub fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Why a job started or was checked for execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    Manual,
    Scheduled,
    Dependency,
    Cascade,
}

/// The terminal outcome of a job attempt. The executor owns detailed output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Failed { exit_code: Option<i32> },
    Blocked,
    Cancelled,
}

/// Why a predecessor did not satisfy a dependency.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    NeverRan,
    NotSuccessful(RunOutcome),
    TooOld,
    NotSinceLastRun,
}

/// One unmet condition, referring to a predecessor or a failed cascade ancestor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Blocker {
    pub predecessor: JobId,
    pub reason: BlockReason,
}

/// A cancellation signal shared with the caller's executor.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Whether cancellation has been requested. The executor owns interruption.
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn request(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Work reserved by the scheduler for a caller-supplied executor.
#[derive(Debug)]
pub struct RunRequest<A> {
    pub id: RunId,
    pub job: JobId,
    pub action: Arc<A>,
    pub trigger: RunTrigger,
    pub started_at: SystemTime,
    pub cancellation: Cancellation,
}

/// A finished run to persist or show in a journal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub id: RunId,
    pub job: JobId,
    pub job_name: String,
    pub trigger: RunTrigger,
    #[serde(with = "record_time")]
    pub started_at: SystemTime,
    #[serde(with = "record_time")]
    pub finished_at: SystemTime,
    pub outcome: RunOutcome,
    pub blockers: Box<[Blocker]>,
    pub(crate) counted_as_run: bool,
}

mod record_time {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        time: &SystemTime,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        timestamp(*time)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SystemTime, D::Error> {
        Timestamp::deserialize(deserializer).map(SystemTime::from)
    }
}

/// State changes from one scheduler operation. The caller dispatches every
/// `started` request exactly once and submits its eventual outcome.
#[derive(Debug)]
#[must_use]
pub struct RunBatch<A> {
    pub started: Vec<RunRequest<A>>,
    pub finished: Vec<RunRecord>,
}

impl<A> Default for RunBatch<A> {
    fn default() -> Self {
        Self {
            started: Vec::new(),
            finished: Vec::new(),
        }
    }
}

#[derive(Default)]
struct JobState {
    active: Option<ActiveRun>,
    last_finished: Option<RunRecord>,
    last_run_at: Option<SystemTime>,
    reserved_by: Option<RunId>,
    next_due: Option<SystemTime>,
}

struct ActiveRun {
    id: RunId,
    trigger: RunTrigger,
    started_at: SystemTime,
    cancellation: Cancellation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Excluded,
    Waiting,
    Running,
    Succeeded,
    Stopped,
}

struct Cascade {
    order: Vec<usize>,
    stages: Vec<Stage>,
}

/// Job runtime with an explicit caller-owned clock and executor. One owner
/// serializes state transitions and can dispatch work to multiple threads.
pub struct Scheduler<A> {
    pub(crate) graph: JobGraph<A>,
    states: Vec<JobState>,
    active: HashMap<RunId, usize>,
    cascades: HashMap<RunId, Cascade>,
    draining: bool,
    zone: Option<TimeZone>,
}

impl<A> Scheduler<A> {
    /// Creates an idle runtime for the given definitions.
    pub fn new(graph: JobGraph<A>) -> Self {
        let states = (0..graph.jobs.len()).map(|_| JobState::default()).collect();
        Self {
            graph,
            states,
            active: HashMap::new(),
            cascades: HashMap::new(),
            draining: false,
            zone: None,
        }
    }

    /// Returns the active job definitions.
    pub fn graph(&self) -> &JobGraph<A> {
        &self.graph
    }

    /// Recomputes schedules strictly after `now`, dropping missed occurrences.
    /// Call after loading definitions or when the system time zone changes.
    /// `TimeZone::try_system()` obtains the host's configured time zone.
    pub fn initialize_schedules(
        &mut self,
        zone: TimeZone,
        now: SystemTime,
    ) -> Result<(), ScheduleError> {
        let after = timestamp(now)?;
        for (job, state) in self.graph.jobs.iter().zip(&mut self.states) {
            state.next_due = deadline(job, &zone, after);
        }
        self.zone = Some(zone);
        Ok(())
    }

    /// Returns the earliest deadline that can dispatch work. A host can park
    /// without a timer when no such job exists.
    pub fn next_due(&self) -> Option<SystemTime> {
        if self.draining {
            return None;
        }
        self.states
            .iter()
            .filter_map(|state| {
                if state.active.is_none() && state.reserved_by.is_none() {
                    state.next_due
                } else {
                    None
                }
            })
            .min()
    }

    /// Returns this job's stored deadline, including while a run is active.
    pub fn scheduled_at(&self, job: &JobId) -> Option<SystemTime> {
        self.states[self.graph.index(job)?].next_due
    }

    /// Attempts each due idle job once. A successful dispatch rearms when the
    /// run finishes; a blocked dispatch rearms immediately. There is no backlog
    /// replay if the host wakes after multiple occurrences.
    pub fn tick(&mut self, now: SystemTime) -> Result<RunBatch<A>, SchedulerError> {
        let mut batch = RunBatch::default();
        if self.draining || self.zone.is_none() {
            return Ok(batch);
        }
        let after = self.calendar_instant(now)?;
        for index in 0..self.states.len() {
            let state = &self.states[index];
            if state.active.is_none()
                && state.reserved_by.is_none()
                && state.next_due.is_some_and(|due| due <= now)
            {
                self.attempt_scheduled(index, now, after, &mut batch);
            }
        }
        Ok(batch)
    }

    /// Returns the latest finished entry for this job, if any.
    pub fn last_finished(&self, job: &JobId) -> Option<&RunRecord> {
        self.states[self.graph.index(job)?].last_finished.as_ref()
    }

    /// Whether this job already has a dispatched run.
    pub fn is_running(&self, job: &JobId) -> bool {
        self.graph
            .index(job)
            .is_some_and(|index| self.states[index].active.is_some())
    }

    /// Checks current gate conditions without starting a run. A manual start
    /// can use this to present the predecessor failures before overriding them.
    pub fn blockers(&self, job: &JobId, now: SystemTime) -> Result<Vec<Blocker>, SchedulerError> {
        let index = self.index(job)?;
        Ok(self.check(
            index,
            self.graph.predecessors[index].iter().copied(),
            now,
            false,
        ))
    }

    /// Starts a job explicitly, bypassing predecessor and enabled checks.
    pub fn start_manual(
        &mut self,
        job: &JobId,
        now: SystemTime,
    ) -> Result<RunBatch<A>, SchedulerError> {
        let index = self.index(job)?;
        self.ensure_available(index)?;
        let mut batch = RunBatch::default();
        self.start(index, RunTrigger::Manual, now, &mut batch);
        Ok(batch)
    }

    /// Attempts an automatic scheduled run. An unmet condition produces a
    /// blocked record without dispatching any work.
    pub fn start_scheduled(
        &mut self,
        job: &JobId,
        now: SystemTime,
    ) -> Result<RunBatch<A>, SchedulerError> {
        let index = self.index(job)?;
        self.ensure_available(index)?;
        if !self.graph.jobs[index].enabled {
            return Err(SchedulerError::Disabled(job.clone()));
        }
        let after = self.calendar_instant(now)?;
        let mut batch = RunBatch::default();
        self.attempt_scheduled(index, now, after, &mut batch);
        Ok(batch)
    }

    /// Starts a manual root, then runs enabled descendants in dependency order.
    /// Branches with disabled descendants are omitted. Overlapping cascades or
    /// active jobs are rejected before any job is started.
    pub fn start_cascade(
        &mut self,
        job: &JobId,
        now: SystemTime,
    ) -> Result<RunBatch<A>, SchedulerError> {
        let root = self.index(job)?;
        self.ensure_available(root)?;
        let mut included = self.graph.descendants(root);
        for &index in &self.graph.topological {
            if index != root && included[index] && !self.graph.jobs[index].enabled {
                for (included, descendant) in included.iter_mut().zip(self.graph.descendants(index))
                {
                    *included &= !descendant;
                }
            }
        }
        let order: Vec<_> = self
            .graph
            .topological
            .iter()
            .copied()
            .filter(|&index| included[index])
            .collect();
        for &index in &order {
            self.ensure_available(index)?;
        }
        let mut batch = RunBatch::default();
        let id = self.start(root, RunTrigger::Cascade, now, &mut batch);
        let mut stages = vec![Stage::Excluded; self.states.len()];
        for &index in &order {
            self.states[index].reserved_by = Some(id);
            stages[index] = Stage::Waiting;
        }
        stages[root] = Stage::Running;
        self.cascades.insert(id, Cascade { order, stages });
        Ok(batch)
    }

    /// Completes one active ticket. Stale or duplicate completions are rejected.
    /// Any released successor starts appear in the returned batch.
    pub fn complete(
        &mut self,
        id: RunId,
        outcome: RunOutcome,
        now: SystemTime,
    ) -> Result<RunBatch<A>, SchedulerError> {
        let index = *self
            .active
            .get(&id)
            .ok_or(SchedulerError::InactiveRun(id))?;
        let next_due = if self.draining {
            None
        } else {
            self.next_for(index, self.calendar_instant(now)?)
        };
        let state = &mut self.states[index];
        let run = state.active.take().ok_or(SchedulerError::InactiveRun(id))?;
        self.active.remove(&id);
        state.next_due = next_due;
        let outcome = if run.cancellation.is_requested() {
            RunOutcome::Cancelled
        } else {
            outcome
        };
        let record = RunRecord {
            id: run.id,
            job: self.graph.jobs[index].id().clone(),
            job_name: self.graph.jobs[index].name().to_owned(),
            trigger: run.trigger,
            started_at: run.started_at,
            finished_at: now,
            outcome,
            blockers: Box::new([]),
            counted_as_run: true,
        };
        let mut batch = RunBatch::default();
        self.store(index, record, &mut batch);
        if !self.draining {
            if let Some(cascade) = self.states[index].reserved_by {
                self.advance_cascade(cascade, index, outcome, now, &mut batch);
            }
            if outcome == RunOutcome::Success {
                self.start_chains(index, now, &mut batch);
            }
        }
        Ok(batch)
    }

    /// Signals the executor. The job remains active until it is completed.
    pub fn cancel(&self, id: RunId) -> Result<(), SchedulerError> {
        let run = self
            .active
            .get(&id)
            .and_then(|&index| self.states[index].active.as_ref())
            .ok_or(SchedulerError::InactiveRun(id))?;
        run.cancellation.request();
        Ok(())
    }

    /// Switches one job and all its descendants. Active executions continue.
    pub fn set_chain_enabled(
        &mut self,
        job: &JobId,
        enabled: bool,
        now: SystemTime,
    ) -> Result<Vec<JobId>, SchedulerError> {
        if self.draining {
            return Err(SchedulerError::Draining);
        }
        let included = self.graph.descendants(self.index(job)?);
        let after = self.calendar_instant(now)?;
        let mut changed = Vec::new();
        for &index in &self.graph.topological {
            let job = &mut self.graph.jobs[index];
            if included[index] && job.enabled != enabled {
                job.enabled = enabled;
                changed.push(job.id().clone());
                self.states[index].next_due = self.next_for(index, after);
            }
        }
        Ok(changed)
    }

    /// Refuses new work and releases all cascade members that have not started.
    /// Active executions remain tracked until their executor completes them.
    pub fn begin_drain(&mut self) {
        self.draining = true;
        self.cascades.clear();
        for state in &mut self.states {
            state.reserved_by = None;
        }
    }

    /// Whether draining was requested and all active work has finished.
    pub fn is_drained(&self) -> bool {
        self.draining && self.active.is_empty()
    }

    fn index(&self, job: &JobId) -> Result<usize, SchedulerError> {
        self.graph
            .index(job)
            .ok_or_else(|| SchedulerError::UnknownJob(job.clone()))
    }

    fn calendar_instant(&self, now: SystemTime) -> Result<Option<Timestamp>, SchedulerError> {
        self.zone
            .as_ref()
            .map(|_| timestamp(now).map_err(SchedulerError::Schedule))
            .transpose()
    }

    fn next_for(&self, index: usize, after: Option<Timestamp>) -> Option<SystemTime> {
        deadline(&self.graph.jobs[index], self.zone.as_ref()?, after?)
    }

    fn attempt_scheduled(
        &mut self,
        index: usize,
        now: SystemTime,
        after: Option<Timestamp>,
        batch: &mut RunBatch<A>,
    ) {
        let blockers = self.check(
            index,
            self.graph.predecessors[index].iter().copied(),
            now,
            false,
        );
        if blockers.is_empty() {
            self.start(index, RunTrigger::Scheduled, now, batch);
        } else {
            self.states[index].next_due = self.next_for(index, after);
            self.record_blocked(index, RunTrigger::Scheduled, now, blockers, batch);
        }
    }

    fn ensure_available(&self, index: usize) -> Result<(), SchedulerError> {
        if self.draining {
            return Err(SchedulerError::Draining);
        }
        let state = &self.states[index];
        if state.active.is_some() || state.reserved_by.is_some() {
            return Err(SchedulerError::Busy(self.graph.jobs[index].id().clone()));
        }
        Ok(())
    }

    fn start(
        &mut self,
        index: usize,
        trigger: RunTrigger,
        now: SystemTime,
        batch: &mut RunBatch<A>,
    ) -> RunId {
        let id = next_run();
        let cancellation = Cancellation::default();
        let job = &self.graph.jobs[index];
        self.active.insert(id, index);
        self.states[index].active = Some(ActiveRun {
            id,
            trigger,
            started_at: now,
            cancellation: cancellation.clone(),
        });
        batch.started.push(RunRequest {
            id,
            job: job.id().clone(),
            action: Arc::clone(&job.action),
            trigger,
            started_at: now,
            cancellation,
        });
        id
    }

    fn check(
        &self,
        job: usize,
        predecessors: impl Iterator<Item = usize>,
        now: SystemTime,
        chain: bool,
    ) -> Vec<Blocker> {
        let state = &self.states[job];
        let max_age = self.graph.jobs[job].max_age_minutes;
        predecessors
            .filter_map(|predecessor| {
                let reason = match &self.states[predecessor].last_finished {
                    None => BlockReason::NeverRan,
                    Some(run) if run.outcome != RunOutcome::Success => {
                        BlockReason::NotSuccessful(run.outcome)
                    }
                    Some(run)
                        if max_age.is_some_and(|max| {
                            now.duration_since(run.finished_at)
                                .unwrap_or_default()
                                .as_secs()
                                / 60
                                > u64::from(max.get())
                        }) =>
                    {
                        BlockReason::TooOld
                    }
                    Some(run)
                        if chain
                            && state
                                .last_run_at
                                .is_some_and(|last_run| run.finished_at <= last_run) =>
                    {
                        BlockReason::NotSinceLastRun
                    }
                    Some(_) => return None,
                };
                Some(Blocker {
                    predecessor: self.graph.jobs[predecessor].id().clone(),
                    reason,
                })
            })
            .collect()
    }

    fn record_blocked(
        &mut self,
        index: usize,
        trigger: RunTrigger,
        now: SystemTime,
        blockers: Vec<Blocker>,
        batch: &mut RunBatch<A>,
    ) {
        let record = RunRecord {
            id: next_run(),
            job: self.graph.jobs[index].id().clone(),
            job_name: self.graph.jobs[index].name().to_owned(),
            trigger,
            started_at: now,
            finished_at: now,
            outcome: RunOutcome::Blocked,
            blockers: blockers.into_boxed_slice(),
            counted_as_run: trigger == RunTrigger::Scheduled,
        };
        // A scheduled attempt updates lastRunAt. A descendant skipped by a
        // cascade only writes the journal and does not consume a chain round.
        self.store(index, record, batch);
    }

    fn store(&mut self, index: usize, record: RunRecord, batch: &mut RunBatch<A>) {
        let state = &mut self.states[index];
        if record.counted_as_run {
            state.last_run_at = Some(record.finished_at);
        }
        if state
            .last_finished
            .as_ref()
            .is_none_or(|last| record.finished_at >= last.finished_at)
        {
            state.last_finished = Some(record.clone());
        }
        batch.finished.push(record);
    }

    pub(crate) fn restore_history<'a>(
        &mut self,
        last_runs: impl Iterator<Item = (&'a JobId, SystemTime)>,
        journal: &[RunRecord],
    ) {
        for state in &mut self.states {
            state.last_finished = None;
            state.last_run_at = None;
        }
        for (id, finished_at) in last_runs {
            if let Some(index) = self.graph.index(id) {
                self.states[index].last_run_at = Some(finished_at);
            }
        }
        for run in journal {
            if let Some(index) = self.graph.index(&run.job) {
                let last = &mut self.states[index].last_finished;
                if last
                    .as_ref()
                    .is_none_or(|last| run.finished_at >= last.finished_at)
                {
                    *last = Some(run.clone());
                }
            }
        }
    }

    fn start_chains(&mut self, index: usize, now: SystemTime, batch: &mut RunBatch<A>) {
        for successor in self.graph.successors[index].clone() {
            let job = &self.graph.jobs[successor];
            if job.enabled
                && job.mode == DependencyMode::Chain
                && self.ensure_available(successor).is_ok()
                && self
                    .check(
                        successor,
                        self.graph.predecessors[successor].iter().copied(),
                        now,
                        true,
                    )
                    .is_empty()
            {
                self.start(successor, RunTrigger::Dependency, now, batch);
            }
        }
    }

    fn advance_cascade(
        &mut self,
        id: RunId,
        index: usize,
        outcome: RunOutcome,
        now: SystemTime,
        batch: &mut RunBatch<A>,
    ) {
        let Some(mut cascade) = self.cascades.remove(&id) else {
            return;
        };
        if outcome == RunOutcome::Success {
            cascade.stages[index] = Stage::Succeeded;
            self.release_waiting(&mut cascade, now, batch);
        } else {
            cascade.stages[index] = Stage::Stopped;
            self.stop_descendants(&mut cascade, index, Some(outcome), now, batch);
        }
        if cascade
            .stages
            .iter()
            .any(|stage| matches!(stage, Stage::Waiting | Stage::Running))
        {
            self.cascades.insert(id, cascade);
        } else {
            for index in cascade.order {
                self.states[index].reserved_by = None;
            }
        }
    }

    fn release_waiting(&mut self, cascade: &mut Cascade, now: SystemTime, batch: &mut RunBatch<A>) {
        for position in 0..cascade.order.len() {
            let index = cascade.order[position];
            if cascade.stages[index] != Stage::Waiting {
                continue;
            }
            if !self.graph.jobs[index].enabled {
                self.stop_descendants(cascade, index, None, now, batch);
                continue;
            }
            let predecessors = &self.graph.predecessors[index];
            if predecessors.iter().any(|&p| {
                matches!(
                    cascade.stages[p],
                    Stage::Waiting | Stage::Running | Stage::Stopped
                )
            }) {
                continue;
            }
            let outside = predecessors
                .iter()
                .copied()
                .filter(|&p| cascade.stages[p] == Stage::Excluded);
            let blockers = self.check(index, outside, now, false);
            if blockers.is_empty() {
                cascade.stages[index] = Stage::Running;
                self.start(index, RunTrigger::Cascade, now, batch);
            } else {
                cascade.stages[index] = Stage::Stopped;
                self.record_blocked(index, RunTrigger::Cascade, now, blockers, batch);
                self.stop_descendants(cascade, index, Some(RunOutcome::Blocked), now, batch);
            }
        }
    }

    fn stop_descendants(
        &mut self,
        cascade: &mut Cascade,
        root: usize,
        outcome: Option<RunOutcome>,
        now: SystemTime,
        batch: &mut RunBatch<A>,
    ) {
        let descendants = self.graph.descendants(root);
        let predecessor = self.graph.jobs[root].id().clone();
        for &index in &cascade.order {
            if descendants[index] && cascade.stages[index] == Stage::Waiting {
                cascade.stages[index] = Stage::Stopped;
                if let Some(outcome) = outcome {
                    let blocker = Blocker {
                        predecessor: predecessor.clone(),
                        reason: BlockReason::NotSuccessful(outcome),
                    };
                    self.record_blocked(index, RunTrigger::Cascade, now, vec![blocker], batch);
                }
            }
        }
    }
}

fn next_run() -> RunId {
    RunId(NEXT_RUN.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn reserve_run_ids_through(high_watermark: u64) {
    NEXT_RUN.fetch_max(high_watermark.saturating_add(1), Ordering::Relaxed);
}

fn deadline<A>(job: &Job<A>, zone: &TimeZone, after: Timestamp) -> Option<SystemTime> {
    if !job.enabled {
        return None;
    }
    job.schedule()?
        .next_timestamp_after(after, zone)
        .map(SystemTime::from)
}

/// A start, completion, cancellation or configuration change was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchedulerError {
    UnknownJob(JobId),
    Busy(JobId),
    Disabled(JobId),
    InactiveRun(RunId),
    Draining,
    Schedule(ScheduleError),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownJob(job) => write!(formatter, "unknown job {job}"),
            Self::Busy(job) => write!(formatter, "job {job} is running or reserved by a cascade"),
            Self::Disabled(job) => write!(formatter, "job {job} is disabled"),
            Self::InactiveRun(id) => write!(formatter, "job run {id} is no longer active"),
            Self::Draining => formatter.write_str("job scheduler is waiting for active runs"),
            Self::Schedule(error) => error.fmt(formatter),
        }
    }
}

impl Error for SchedulerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Schedule(error) => Some(error),
            _ => None,
        }
    }
}
