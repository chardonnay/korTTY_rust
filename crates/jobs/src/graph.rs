use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

use kortty_diagram::{MAX_EDGES, MAX_LABEL_CHARS, MAX_NODES};
use serde::{Deserialize, Serialize};

use crate::JobSchedule;

/// Upper bound shared with the native dependency diagram.
pub const MAX_JOBS: usize = MAX_NODES;
/// Upper bound on the number of edges in a job graph.
pub const MAX_DEPENDENCIES: usize = MAX_EDGES;
const MAX_ID_BYTES: usize = 64;

/// Stable job identifier usable as a node identifier in a native diagram.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String")]
pub struct JobId(String);

impl JobId {
    /// Accepts 1–64 ASCII letters, digits, underscores or hyphens. A hyphen
    /// cannot be the first character.
    pub fn new(value: impl Into<String>) -> Result<Self, DefinitionError> {
        let value = value.into();
        if value.len() > MAX_ID_BYTES
            || !value
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(DefinitionError::InvalidId);
        }
        Ok(Self(value))
    }

    /// Returns the identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<String> for JobId {
    type Error = DefinitionError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Controls whether a predecessor's success also triggers its dependent job.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyMode {
    /// Checks predecessors when the job's own schedule fires.
    #[default]
    Gate,
    /// Starts after all predecessors have succeeded since this job last ran.
    /// An explicit scheduled start still uses gate semantics.
    Chain,
}

/// Persistable job metadata. The caller resolves execution data by `id`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobDefinition {
    pub id: JobId,
    pub name: String,
    pub enabled: bool,
    pub mode: DependencyMode,
    pub max_age_minutes: Option<NonZeroU32>,
    pub dependencies: Vec<JobId>,
    pub schedule: Option<JobSchedule>,
}

impl JobDefinition {
    /// Attaches execution data from the caller and validates the job's name.
    pub fn with_action<A>(self, action: A) -> Result<Job<A>, DefinitionError> {
        let mut job = Job::new(self.id, self.name, action)?
            .with_enabled(self.enabled)
            .with_dependencies(self.mode, self.dependencies, self.max_age_minutes);
        job.schedule = self.schedule;
        Ok(job)
    }
}

/// A job definition with caller-defined execution data.
#[derive(Debug)]
pub struct Job<A> {
    id: JobId,
    name: String,
    pub(crate) enabled: bool,
    pub(crate) mode: DependencyMode,
    pub(crate) max_age_minutes: Option<NonZeroU32>,
    dependencies: Vec<JobId>,
    pub(crate) schedule: Option<JobSchedule>,
    pub(crate) action: Arc<A>,
}

impl<A> Job<A> {
    /// Creates an enabled job without predecessors. The name must contain
    /// visible text and fit the diagram label budget.
    pub fn new(id: JobId, name: impl Into<String>, action: A) -> Result<Self, DefinitionError> {
        let name = name.into();
        let name = name.trim();
        validate_name(name)?;
        Ok(Self {
            id,
            name: name.to_owned(),
            enabled: true,
            mode: DependencyMode::Gate,
            max_age_minutes: None,
            dependencies: Vec::new(),
            schedule: None,
            action: Arc::new(action),
        })
    }

    /// Sets the initial enabled state.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Opts a job into automatic calendar scheduling.
    #[must_use]
    pub fn with_schedule(mut self, schedule: JobSchedule) -> Self {
        self.schedule = Some(schedule);
        self
    }

    /// The configured calendar schedule, if any.
    pub fn schedule(&self) -> Option<&JobSchedule> {
        self.schedule.as_ref()
    }

    /// Sets predecessor IDs and the optional maximum age in whole minutes.
    /// [`JobGraph::new`] validates the edges against the other definitions.
    #[must_use]
    pub fn with_dependencies(
        mut self,
        mode: DependencyMode,
        predecessors: Vec<JobId>,
        max_age_minutes: Option<NonZeroU32>,
    ) -> Self {
        self.mode = mode;
        self.dependencies = predecessors;
        self.max_age_minutes = max_age_minutes;
        self
    }

    /// Stable identifier.
    pub fn id(&self) -> &JobId {
        &self.id
    }

    /// Display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether automatic and cascade starts may run this job.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Dependency evaluation and triggering mode.
    pub fn mode(&self) -> DependencyMode {
        self.mode
    }

    /// Optional freshness limit in whole minutes.
    pub fn max_age_minutes(&self) -> Option<NonZeroU32> {
        self.max_age_minutes
    }

    /// Predecessor IDs in definition order.
    pub fn dependencies(&self) -> &[JobId] {
        &self.dependencies
    }

    /// Caller-owned execution data.
    pub fn action(&self) -> &A {
        &self.action
    }

    /// Copies only the metadata that can be stored in a scheduler profile.
    pub fn definition(&self) -> JobDefinition {
        JobDefinition {
            id: self.id.clone(),
            name: self.name.clone(),
            enabled: self.enabled,
            mode: self.mode,
            max_age_minutes: self.max_age_minutes,
            dependencies: self.dependencies.clone(),
            schedule: self.schedule.clone(),
        }
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), DefinitionError> {
    if name.trim() != name || name.is_empty() || name.chars().count() > MAX_LABEL_CHARS
        || name.chars().any(|ch| {
            ch.is_control() || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{fffe}' | '\u{ffff}')
        })
    {
        return Err(DefinitionError::InvalidName);
    }
    Ok(())
}

/// Invalid data in one job definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefinitionError {
    InvalidId,
    InvalidName,
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidId => "invalid job identifier",
            Self::InvalidName => "invalid job name",
        })
    }
}

impl Error for DefinitionError {}

/// An immutable dependency topology in stable definition order.
#[derive(Debug)]
pub struct JobGraph<A> {
    pub(crate) jobs: Vec<Job<A>>,
    indices: HashMap<JobId, usize>,
    pub(crate) predecessors: Vec<Vec<usize>>,
    pub(crate) successors: Vec<Vec<usize>>,
    pub(crate) topological: Vec<usize>,
}

impl<A> JobGraph<A> {
    /// Rejects missing or duplicate IDs, self-dependencies and cycles before
    /// accepting the graph. Repeated predecessor IDs are errors.
    pub fn new(jobs: Vec<Job<A>>) -> Result<Self, GraphError> {
        if jobs.len() > MAX_JOBS {
            return Err(GraphError::TooManyJobs);
        }
        let mut indices = HashMap::with_capacity(jobs.len());
        let mut edge_count = 0usize;
        for (index, job) in jobs.iter().enumerate() {
            if indices.insert(job.id.clone(), index).is_some() {
                return Err(GraphError::DuplicateJob(job.id.clone()));
            }
            edge_count = edge_count.saturating_add(job.dependencies.len());
            if edge_count > MAX_DEPENDENCIES {
                return Err(GraphError::TooManyDependencies);
            }
        }
        let mut predecessors = vec![Vec::new(); jobs.len()];
        let mut successors = vec![Vec::new(); jobs.len()];
        for (index, job) in jobs.iter().enumerate() {
            for predecessor in &job.dependencies {
                let Some(&source) = indices.get(predecessor) else {
                    return Err(GraphError::UnknownPredecessor {
                        job: job.id.clone(),
                        predecessor: predecessor.clone(),
                    });
                };
                if source == index {
                    return Err(GraphError::SelfDependency(job.id.clone()));
                }
                if predecessors[index].contains(&source) {
                    return Err(GraphError::DuplicateDependency {
                        job: job.id.clone(),
                        predecessor: predecessor.clone(),
                    });
                }
                predecessors[index].push(source);
                successors[source].push(index);
            }
        }
        let mut pending: Vec<_> = predecessors.iter().map(Vec::len).collect();
        let mut ready: VecDeque<_> = (0..jobs.len())
            .filter(|&index| pending[index] == 0)
            .collect();
        let mut topological = Vec::with_capacity(jobs.len());
        while let Some(index) = ready.pop_front() {
            topological.push(index);
            for &next in &successors[index] {
                pending[next] -= 1;
                if pending[next] == 0 {
                    ready.push_back(next);
                }
            }
        }
        if topological.len() != jobs.len() {
            return Err(GraphError::Cycle);
        }
        Ok(Self {
            jobs,
            indices,
            predecessors,
            successors,
            topological,
        })
    }

    /// Jobs in definition order.
    pub fn jobs(&self) -> &[Job<A>] {
        &self.jobs
    }

    /// Looks up a job by identifier.
    pub fn job(&self, id: &JobId) -> Option<&Job<A>> {
        self.index(id).map(|index| &self.jobs[index])
    }

    /// Returns the selected job and all its descendants in dependency order.
    pub fn chain(&self, id: &JobId) -> Option<Vec<&Job<A>>> {
        let members = self.descendants(self.index(id)?);
        Some(
            self.topological
                .iter()
                .filter_map(|&index| members[index].then_some(&self.jobs[index]))
                .collect(),
        )
    }

    /// Returns jobs the selected job could depend on without closing a cycle.
    pub fn allowed_predecessors(&self, id: &JobId) -> Vec<&Job<A>> {
        let excluded = self.index(id).map(|index| self.descendants(index));
        self.jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                excluded
                    .as_ref()
                    .is_none_or(|excluded| !excluded[index])
                    .then_some(job)
            })
            .collect()
    }

    pub(crate) fn index(&self, id: &JobId) -> Option<usize> {
        self.indices.get(id).copied()
    }

    pub(crate) fn descendants(&self, root: usize) -> Vec<bool> {
        reach(root, &self.successors)
    }

    pub(crate) fn ancestors(&self, root: usize) -> Vec<bool> {
        reach(root, &self.predecessors)
    }
}

fn reach(root: usize, edges: &[Vec<usize>]) -> Vec<bool> {
    let mut found = vec![false; edges.len()];
    let mut pending = vec![root];
    found[root] = true;
    while let Some(index) = pending.pop() {
        for &next in &edges[index] {
            if !found[next] {
                found[next] = true;
                pending.push(next);
            }
        }
    }
    found
}

/// Inconsistent or oversized job dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphError {
    TooManyJobs,
    TooManyDependencies,
    DuplicateJob(JobId),
    UnknownPredecessor { job: JobId, predecessor: JobId },
    DuplicateDependency { job: JobId, predecessor: JobId },
    SelfDependency(JobId),
    Cycle,
}

impl fmt::Display for GraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyJobs => write!(formatter, "the job graph exceeds {MAX_JOBS} jobs"),
            Self::TooManyDependencies => write!(
                formatter,
                "the job graph exceeds {MAX_DEPENDENCIES} dependencies"
            ),
            Self::DuplicateJob(id) => write!(formatter, "duplicate job {id}"),
            Self::UnknownPredecessor { job, predecessor } => {
                write!(formatter, "job {job} depends on unknown job {predecessor}")
            }
            Self::DuplicateDependency { job, predecessor } => {
                write!(formatter, "job {job} repeats predecessor {predecessor}")
            }
            Self::SelfDependency(id) => write!(formatter, "job {id} depends on itself"),
            Self::Cycle => formatter.write_str("job dependencies form a cycle"),
        }
    }
}

impl Error for GraphError {}
