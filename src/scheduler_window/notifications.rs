//! Per-job desktop notices derived from committed run records.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use iced::Element;
use iced::widget::{checkbox, column, row};
use kortty_jobs::{JobId, RunOutcome, RunRecord, RunTrigger};
use serde::{Deserialize, Serialize};

const COOLDOWN: Duration = Duration::from_secs(60);
const MAX_PENDING: usize = 128;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    desktop: bool,
    failed: bool,
    blocked: bool,
    recovered: bool,
    success: bool,
    cancelled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            desktop: true,
            failed: true,
            blocked: true,
            recovered: false,
            success: false,
            cancelled: false,
        }
    }
}

impl Settings {
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn includes(&self, outcome: RunOutcome, recovered: bool) -> bool {
        self.desktop && self.matches(outcome, recovered)
    }

    pub(super) fn matches(&self, outcome: RunOutcome, recovered: bool) -> bool {
        match outcome {
            RunOutcome::Success => self.success || (self.recovered && recovered),
            RunOutcome::Failed { .. } => self.failed,
            RunOutcome::Blocked => self.blocked,
            RunOutcome::Cancelled => self.cancelled,
        }
    }

    pub(super) fn view(self, has_webhooks: bool) -> Element<'static, super::Message> {
        use super::Message::DesktopNotices;

        let option = |label, checked, updated: fn(&mut Self, bool)| {
            checkbox(checked).label(label).on_toggle_maybe(
                (self.desktop || has_webhooks).then_some(move |value| {
                    let mut next = self;
                    updated(&mut next, value);
                    DesktopNotices(next)
                }),
            )
        };
        column![
            checkbox(self.desktop)
                .label("Show desktop notifications")
                .on_toggle(move |desktop| DesktopNotices(Self { desktop, ..self })),
            row![
                option("Failed", self.failed, |next, value| next.failed = value),
                option("Blocked", self.blocked, |next, value| next.blocked = value),
                option("Recovered", self.recovered, |next, value| next.recovered =
                    value),
                option("Successful", self.success, |next, value| next.success =
                    value),
                option("Cancelled", self.cancelled, |next, value| next.cancelled =
                    value),
            ]
            .spacing(12)
            .wrap(),
        ]
        .spacing(6)
        .into()
    }
}

pub(crate) struct Notice {
    job: JobId,
    outcome: RunOutcome,
    recovered: bool,
    pub(crate) title: String,
    pub(crate) body: String,
}

impl Notice {
    fn new(run: &RunRecord, recovered: bool) -> Self {
        let status = match run.outcome {
            RunOutcome::Success if recovered => "Recovered",
            RunOutcome::Success => "Successful",
            RunOutcome::Failed { .. } => "Failed",
            RunOutcome::Blocked => "Blocked",
            RunOutcome::Cancelled => "Cancelled",
        };
        let trigger = match run.trigger {
            RunTrigger::Manual => "Manual run",
            RunTrigger::Scheduled => "Scheduled run",
            RunTrigger::Dependency => "Dependency run",
            RunTrigger::Cascade => "Cascade run",
        };
        let body = match run.outcome {
            RunOutcome::Failed {
                exit_code: Some(code),
            } => {
                format!("{status} · Exit code {code} · {trigger}")
            }
            _ => format!("{status} · {trigger}"),
        };
        Self {
            job: run.job.clone(),
            outcome: run.outcome,
            recovered,
            title: format!("Job {}", run.job_name),
            body,
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pending: VecDeque<Notice>,
    last_sent: BTreeMap<JobId, Instant>,
}

impl State {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn prepare(
        store: &kortty_jobs::SchedulerStore,
        graph: &kortty_jobs::JobGraph<super::SshCommand>,
        records: &[RunRecord],
    ) -> Vec<Notice> {
        let mut notices = Vec::new();
        for (index, record) in records.iter().enumerate() {
            let Some(job) = graph.job(&record.job) else {
                continue;
            };
            let settings = job.action().desktop_notices;
            if !settings.desktop || store.journal_record(record.id).is_some() {
                continue;
            }
            let recovered = (settings.success || settings.recovered)
                && recovered(store, record, &records[..index]);
            if settings.includes(record.outcome, recovered) {
                notices.push(Notice::new(record, recovered));
            }
        }
        notices
    }

    pub(super) fn committed(&mut self, notices: Vec<Notice>) {
        self.pending.extend(notices);
        let excess = self.pending.len().saturating_sub(MAX_PENDING);
        self.pending.drain(..excess);
    }

    pub(super) fn take(
        &mut self,
        now: Instant,
        allowed: bool,
        graph: &kortty_jobs::JobGraph<super::SshCommand>,
    ) -> Vec<Notice> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        if !allowed {
            self.pending.clear();
            return Vec::new();
        }
        self.last_sent.retain(|id, last| {
            now.saturating_duration_since(*last) < COOLDOWN && graph.job(id).is_some()
        });
        self.pending
            .drain(..)
            .filter(|notice| {
                let Some(job) = graph.job(&notice.job) else {
                    return false;
                };
                if !job
                    .action()
                    .desktop_notices
                    .includes(notice.outcome, notice.recovered)
                {
                    return false;
                }
                match self.last_sent.entry(notice.job.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(now);
                        true
                    }
                    std::collections::btree_map::Entry::Occupied(_) => false,
                }
            })
            .collect()
    }
}

pub(super) fn recovered(
    store: &kortty_jobs::SchedulerStore,
    record: &RunRecord,
    earlier: &[RunRecord],
) -> bool {
    record.outcome == RunOutcome::Success
        && store
            .latest_finished(&record.job)
            .into_iter()
            .chain(earlier.iter().filter(|run| run.job == record.job))
            .max_by_key(|run| run.finished_at)
            .is_some_and(|run| {
                matches!(run.outcome, RunOutcome::Failed { .. } | RunOutcome::Blocked)
            })
}
