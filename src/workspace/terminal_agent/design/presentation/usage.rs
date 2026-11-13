//! Ephemeral reported model counts. This does not write to or charge the usage ledger.

use std::collections::VecDeque;

use crate::terminal_agent::usage::{Attempt, AttemptKind, AttemptOutcome};

const MAX_ATTEMPTS: usize = 32;

#[derive(Default)]
pub(in crate::workspace::terminal_agent) struct Usage {
    epoch: Option<u64>,
    seen: [bool; 2],
    attempts: u64,
    input: Count,
    output: Count,
    total: Count,
    records: VecDeque<Record>,
}

enum Record {
    Reported { epoch: u64, attempt: Attempt },
    Repair { epoch: u64, stopped: Option<bool> },
}

impl Record {
    fn key(&self) -> (u64, AttemptKind) {
        match self {
            Self::Reported { epoch, attempt } => (*epoch, attempt.kind),
            Self::Repair { epoch, .. } => (*epoch, AttemptKind::Repair),
        }
    }

    fn detail(&self) -> String {
        let attempt =
            match self {
                Self::Reported { attempt, .. } => attempt,
                Self::Repair { stopped, .. } => return match stopped {
                    None => "AI response repair · Running\nRepairing the AI response format.",
                    Some(true) => {
                        "AI response repair · Cancelled\nNo completed repair report is available."
                    }
                    Some(false) => {
                        "AI response repair · Failed\nNo completed repair report is available."
                    }
                }
                .into(),
            };
        let kind = match attempt.kind {
            AttemptKind::Initial => "AI decision",
            AttemptKind::Repair => "AI response repair",
        };
        let outcome = match attempt.outcome {
            AttemptOutcome::Completed => "Completed",
            AttemptOutcome::InvalidReply => "Invalid reply",
            AttemptOutcome::TokenLimit => "Token limit",
            AttemptOutcome::LikelyTokenLimit => "Likely token limit",
            AttemptOutcome::Failed => "Failed",
            AttemptOutcome::Cancelled => "Cancelled",
        };
        let field = |value: Option<u64>, name: &str| {
            value.map_or_else(
                || format!("{name} unavailable"),
                |value| format!("{value} {name}"),
            )
        };
        format!(
            "{kind} · {outcome} · {}.{:03} s\nReported tokens · {} · {} · {}",
            attempt.elapsed.as_secs(),
            attempt.elapsed.subsec_millis(),
            field(attempt.input_tokens, "input"),
            field(attempt.output_tokens, "output"),
            field(attempt.total_tokens, "total")
        )
    }
}

#[derive(Clone, Copy, Default)]
pub(in crate::workspace::terminal_agent) struct Count {
    pub(in crate::workspace::terminal_agent) sum: u128,
    pub(in crate::workspace::terminal_agent) known: u64,
}

#[derive(Clone, Copy)]
pub(in crate::workspace::terminal_agent) struct Summary {
    pub(in crate::workspace::terminal_agent) attempts: u64,
    pub(in crate::workspace::terminal_agent) input: Count,
    pub(in crate::workspace::terminal_agent) output: Count,
    pub(in crate::workspace::terminal_agent) total: Count,
}

impl Count {
    fn add(&mut self, value: Option<u64>) {
        if let Some(value) = value {
            // At most u64::MAX accepted attempts, each containing at most u64::MAX tokens.
            self.sum += u128::from(value);
            self.known += 1;
        }
    }

    fn label(&self, name: &str, attempts: u64) -> String {
        if self.known == 0 {
            format!("{name} unavailable")
        } else if self.known < attempts {
            format!(
                "{} {name} (reported subtotal; {}/{attempts} attempts)",
                self.sum, self.known
            )
        } else {
            format!("{} {name}", self.sum)
        }
    }
}

impl Usage {
    pub(in crate::workspace::terminal_agent) fn summary(&self) -> Option<Summary> {
        (self.attempts > 0).then_some(Summary {
            attempts: self.attempts,
            input: self.input,
            output: self.output,
            total: self.total,
        })
    }
    /// The caller has already checked run, source and epoch. Older or duplicate events stay inert.
    pub(in crate::workspace::terminal_agent) fn accept(
        &mut self,
        epoch: u64,
        attempt: &Attempt,
    ) -> bool {
        if self.epoch.is_some_and(|current| epoch < current) || self.attempts == u64::MAX {
            return false;
        }
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.seen = [false; 2];
        }
        let index = match attempt.kind {
            AttemptKind::Initial => 0,
            AttemptKind::Repair => 1,
        };
        if self.seen[index] {
            return false;
        }
        self.seen[index] = true;
        self.attempts += 1;
        self.input.add(attempt.input_tokens);
        self.output.add(attempt.output_tokens);
        self.total.add(attempt.total_tokens);
        let record = Record::Reported {
            epoch,
            attempt: attempt.clone(),
        };
        if let Some(existing) = self
            .records
            .iter_mut()
            .find(|entry| entry.key() == record.key())
        {
            *existing = record;
        } else {
            self.push(record);
        }
        true
    }

    fn push(&mut self, record: Record) {
        if self.records.len() == MAX_ATTEMPTS {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    /// Only a real completed initial attempt may precede this presentation-only row.
    pub(in crate::workspace::terminal_agent) fn start_repair(&mut self, epoch: u64) -> bool {
        if self.epoch != Some(epoch)
            || !self.seen[0]
            || self.seen[1]
            || self
                .records
                .iter()
                .any(|record| record.key() == (epoch, AttemptKind::Repair))
        {
            return false;
        }
        self.push(Record::Repair {
            epoch,
            stopped: None,
        });
        true
    }

    pub(in crate::workspace::terminal_agent) fn finish_repair(
        &mut self,
        epoch: u64,
        cancelled: bool,
    ) -> bool {
        let Some(Record::Repair { stopped, .. }) = self
            .records
            .iter_mut()
            .find(|record| record.key() == (epoch, AttemptKind::Repair))
        else {
            return false;
        };
        if stopped.is_some() {
            return false;
        }
        *stopped = Some(cancelled);
        true
    }

    pub(in crate::workspace::terminal_agent) fn repairing(&self, epoch: u64) -> bool {
        self.records.iter().any(|record| matches!(record, Record::Repair { epoch: current, stopped: None } if *current == epoch))
    }

    pub(in crate::workspace::terminal_agent) fn row(
        &self,
        epoch: u64,
        kind: AttemptKind,
    ) -> Option<String> {
        self.records
            .iter()
            .find(|record| record.key() == (epoch, kind))
            .map(Record::detail)
    }

    pub(in crate::workspace::terminal_agent) fn attempts(
        &self,
    ) -> impl DoubleEndedIterator<Item = &Attempt> {
        self.records.iter().filter_map(|record| match record {
            Record::Reported { attempt, .. } => Some(attempt),
            Record::Repair { .. } => None,
        })
    }

    pub(in crate::workspace::terminal_agent) fn rows(
        &self,
    ) -> impl DoubleEndedIterator<Item = String> + '_ {
        self.records.iter().map(Record::detail)
    }

    pub(in crate::workspace::terminal_agent) fn total(&self) -> Option<u64> {
        (self.attempts > 0 && self.total.known == self.attempts)
            .then(|| u64::try_from(self.total.sum).ok())
            .flatten()
    }

    pub(in crate::workspace::terminal_agent) fn label(&self) -> Option<String> {
        (self.attempts > 0).then(|| {
            format!(
                "Reported tokens · {} · {} · {}",
                self.input.label("input", self.attempts),
                self.output.label("output", self.attempts),
                self.total.label("total", self.attempts)
            )
        })
    }

    pub(in crate::workspace::terminal_agent) fn compact(&self) -> Option<String> {
        if self.attempts == 0 {
            None
        } else if self.total.known == 0 {
            Some("Total tokens unavailable".into())
        } else if self.total.known < self.attempts {
            Some(format!("{} tokens (reported subtotal)", self.total.sum))
        } else {
            Some(format!("{} reported tokens", self.total.sum))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_agent::usage::AttemptOutcome;
    use std::time::Duration;

    fn attempt(
        kind: AttemptKind,
        input: Option<u64>,
        output: Option<u64>,
        total: Option<u64>,
    ) -> Attempt {
        Attempt {
            kind,
            outcome: AttemptOutcome::Completed,
            elapsed: Duration::from_millis(10),
            input_tokens: input,
            output_tokens: output,
            total_tokens: total,
        }
    }

    #[test]
    fn independent_reporting_coverage_preserves_zero_and_rejects_duplicate_or_old_attempts() {
        let mut usage = Usage::default();
        assert!(usage.label().is_none());
        let initial = attempt(AttemptKind::Initial, Some(0), None, None);
        assert!(usage.accept(4, &initial));
        assert_eq!(
            usage.label().as_deref(),
            Some("Reported tokens · 0 input · output unavailable · total unavailable")
        );
        assert!(!usage.accept(4, &initial));
        assert!(usage.accept(
            4,
            &attempt(AttemptKind::Repair, Some(12), Some(3), Some(15))
        ));
        assert_eq!(
            usage.label().as_deref(),
            Some(
                "Reported tokens · 12 input · 3 output (reported subtotal; 1/2 attempts) · 15 total (reported subtotal; 1/2 attempts)"
            )
        );
        assert!(usage.total().is_none());
        assert_eq!(
            usage.compact().as_deref(),
            Some("15 tokens (reported subtotal)")
        );
        assert!(usage.accept(9, &attempt(AttemptKind::Initial, None, Some(0), None)));
        assert!(!usage.accept(4, &initial));
        assert_eq!(usage.attempts, 3);
        assert_eq!(usage.output.known, 2);
    }

    #[test]
    fn bounded_attempt_history_retains_lifetime_totals_after_eviction() {
        let mut usage = Usage::default();
        for epoch in 1..=100 {
            assert!(usage.accept(
                epoch,
                &attempt(AttemptKind::Initial, Some(1), Some(1), Some(2))
            ));
        }
        assert_eq!(usage.attempts().count(), MAX_ATTEMPTS);
        assert_eq!(usage.total(), Some(200));
        let summary = usage.summary().unwrap();
        assert_eq!(summary.attempts, 100);
        assert_eq!((summary.input.sum, summary.input.known), (100, 100));
        assert_eq!((summary.output.sum, summary.output.known), (100, 100));
        assert_eq!((summary.total.sum, summary.total.known), (200, 100));
        assert!(!usage.accept(
            1,
            &attempt(AttemptKind::Initial, Some(100), Some(100), Some(200))
        ));
        assert_eq!(usage.total(), Some(200));
    }

    #[test]
    fn live_repair_reserves_a_bounded_row_without_reserving_usage_or_double_counting_completion() {
        let mut usage = Usage::default();
        assert!(!usage.start_repair(100));
        for epoch in 1..=100 {
            assert!(usage.accept(
                epoch,
                &attempt(AttemptKind::Initial, Some(1), Some(1), Some(2))
            ));
        }
        assert!(!usage.start_repair(99));
        assert!(usage.start_repair(100));
        assert!(!usage.start_repair(100));
        assert_eq!(usage.rows().count(), MAX_ATTEMPTS);
        assert_eq!(usage.attempts().count(), MAX_ATTEMPTS - 1);
        assert_eq!(usage.summary().unwrap().attempts, 100);
        assert_eq!(usage.total(), Some(200));
        assert!(usage.rows().next_back().unwrap().contains("Running"));
        assert!(!usage.finish_repair(99, true));
        assert!(usage.finish_repair(100, true));
        assert!(!usage.finish_repair(100, false));
        assert!(usage.rows().next_back().unwrap().contains("Cancelled"));
        assert_eq!(usage.summary().unwrap().attempts, 100);
        let repair = attempt(AttemptKind::Repair, Some(0), None, None);
        assert!(usage.accept(100, &repair));
        assert!(!usage.accept(100, &repair));
        assert!(!usage.start_repair(100));
        assert_eq!(usage.rows().count(), MAX_ATTEMPTS);
        assert_eq!(usage.attempts().count(), MAX_ATTEMPTS);
        let summary = usage.summary().unwrap();
        assert_eq!(summary.attempts, 101);
        assert_eq!((summary.input.sum, summary.input.known), (100, 101));
        assert_eq!((summary.total.sum, summary.total.known), (200, 100));
        assert!(usage.total().is_none());
        assert!(usage.rows().next_back().unwrap().contains("Completed"));
    }

    #[test]
    fn complete_zero_and_large_totals_never_wrap_or_become_partial_by_accident() {
        let mut usage = Usage::default();
        assert!(usage.accept(1, &attempt(AttemptKind::Initial, Some(0), Some(0), Some(0))));
        assert_eq!(usage.total(), Some(0));
        assert_eq!(usage.compact().as_deref(), Some("0 reported tokens"));
        assert!(usage.accept(
            2,
            &attempt(
                AttemptKind::Initial,
                Some(u64::MAX),
                Some(0),
                Some(u64::MAX)
            )
        ));
        assert!(usage.accept(2, &attempt(AttemptKind::Repair, Some(1), Some(0), Some(1))));
        assert_eq!(
            usage.compact().as_deref(),
            Some("18446744073709551616 reported tokens")
        );
        assert!(
            usage.total().is_none(),
            "the narrow terminal total cannot wrap"
        );
    }
}
