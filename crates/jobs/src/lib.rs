//! Validated job dependencies, execution state and native diagrams for kortty.
//!
//! A [`Scheduler`] uses the caller's clock and executor. [`SchedulerStore`]
//! persists definitions and structured outcomes, leaving executor data with
//! the caller.

#![forbid(unsafe_code)]

mod diagram;
mod graph;
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
pub use schedule::{JobSchedule, SCHEDULE_SEARCH_DAYS, ScheduleError};
pub use scheduler::{
    BlockReason, Blocker, Cancellation, RunBatch, RunId, RunOutcome, RunRecord, RunRequest,
    RunTrigger, Scheduler, SchedulerError,
};
pub use store::{
    JournalRecording, MAX_JOURNAL_RECORDS, MAX_STORE_BYTES, SchedulerStore, StoreError,
};
