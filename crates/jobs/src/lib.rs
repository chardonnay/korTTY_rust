//! Validated job dependencies, execution state and native diagrams for kortty.
//!
//! A [`Scheduler`] uses the caller's clock and executor. [`SchedulerStore`]
//! persists definitions and structured outcomes, leaving executor data with
//! the caller.

#![forbid(unsafe_code)]

mod diagram;
mod graph;
mod profile_ref;
mod schedule;
mod scheduler;
mod store;

pub use diagram::DiagramScope;
pub use graph::{
    DefinitionError, DependencyMode, GraphError, Job, JobDefinition, JobGraph, JobId,
    MAX_DEPENDENCIES, MAX_JOBS,
};
pub use jiff::{
    civil::{Date, Time, Weekday},
    tz::TimeZone,
};
pub use profile_ref::{MAX_POLICY_PROFILE_ID_BYTES, ProfileRef, valid_policy_ai_profile_id};
pub use schedule::{JobSchedule, SCHEDULE_SEARCH_DAYS, ScheduleError};
pub use scheduler::{
    BlockReason, Blocker, Cancellation, CascadeOrigin, RunBatch, RunId, RunOutcome, RunRecord,
    RunRequest, RunTrigger, Scheduler, SchedulerError,
};
pub use store::{
    FinishedRunMetadata, JournalRecording, MAX_JOURNAL_RECORDS, MAX_SESSION_JOURNALS_PER_RUN,
    MAX_STORE_BYTES, MAX_SWARM_COMPARISON_BYTES, MAX_SWARM_QUESTION_BYTES,
    MAX_SWARM_TARGET_DETAIL_BYTES, MAX_SWARM_TARGET_LABEL_BYTES, MAX_SWARM_TARGETS,
    MAX_WEBHOOKS_PER_RUN, PreparedJobAdditions, SchedulerStore, StoreError, StoreLock,
    SwarmChatOrigin, SwarmChatSeed, SwarmReport, SwarmTargetReport, WebhookOutcome,
    valid_session_journal_id, valid_session_journal_ids, valid_swarm_comparison,
};

/// Maximum simultaneously active targets in one scheduled iterative Swarm.
pub const MAX_SWARM_PARALLELISM: usize = 16;
