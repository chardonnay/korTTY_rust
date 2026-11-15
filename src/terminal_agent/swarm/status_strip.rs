use std::time::{Duration, Instant};

use super::{Queue, Row, Status, Ticket};

const INITIAL_SLOW_SECONDS: u64 = 180;
const MINIMUM_SLOW_SECONDS: u64 = 60;

#[derive(Clone)]
pub(super) struct Clock {
    elapsed: Duration,
    running_since: Option<Instant>,
}

impl Clock {
    pub(super) fn start(now: Instant) -> Self {
        Self {
            elapsed: Duration::ZERO,
            running_since: Some(now),
        }
    }

    pub(super) fn elapsed_seconds(&self, now: Instant) -> u64 {
        self.elapsed
            .saturating_add(
                self.running_since
                    .map_or(Duration::ZERO, |since| now.saturating_duration_since(since)),
            )
            .as_secs()
    }

    pub(super) fn is_running(&self) -> bool {
        self.running_since.is_some()
    }

    pub(super) fn set_running(&mut self, now: Instant, running: bool) {
        if running {
            self.running_since.get_or_insert(now);
        } else if let Some(since) = self.running_since.take() {
            self.elapsed = self
                .elapsed
                .saturating_add(now.saturating_duration_since(since));
        }
    }
}

pub(crate) struct StatusStrip<'a> {
    pub(crate) targets: Vec<TargetStatus<'a>>,
}

pub(crate) struct TargetStatus<'a> {
    pub(crate) ticket: Ticket,
    pub(crate) row: &'a Row,
    pub(crate) state: Status,
    pub(crate) elapsed_seconds: Option<u64>,
    pub(crate) slow: bool,
}

impl<'a> StatusStrip<'a> {
    pub(super) fn new(queue: &'a Queue, now: Instant) -> Self {
        let mut successful = queue
            .rows
            .iter()
            .filter(|row| row.status == Status::Succeeded)
            .filter_map(|row| row.elapsed_seconds(now))
            .collect::<Vec<_>>();
        let threshold = slow_threshold(&mut successful);
        let targets = queue
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let state = row.display_status(queue.paused);
                let elapsed_seconds = row.elapsed_seconds(now);
                TargetStatus {
                    ticket: Ticket {
                        round: queue.round,
                        index,
                        key: row.target.key,
                        attempt: row.attempt,
                    },
                    state,
                    elapsed_seconds,
                    slow: state.is_working()
                        && elapsed_seconds.is_some_and(|elapsed| elapsed >= threshold),
                    row,
                }
            })
            .collect();
        Self { targets }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusCounts {
    pub(crate) running: usize,
    pub(crate) review: usize,
    pub(crate) paused: usize,
    pub(crate) slow: usize,
    pub(crate) done: usize,
    pub(crate) failed: usize,
    pub(crate) total: usize,
}

impl StatusCounts {
    pub(crate) fn for_states(states: impl IntoIterator<Item = (Status, bool)>) -> Self {
        let mut counts = Self::default();
        for (status, slow) in states {
            counts.total += 1;
            match status {
                Status::Starting | Status::Running => counts.running += 1,
                Status::Review => counts.review += 1,
                Status::Paused => counts.paused += 1,
                Status::Succeeded => counts.done += 1,
                Status::Failed | Status::Blocked => counts.failed += 1,
                Status::Queued | Status::Cancelling | Status::Cancelled => {}
            }
            if slow && matches!(status, Status::Starting | Status::Running) {
                counts.slow += 1;
            }
        }
        counts
    }
}

fn slow_threshold(successful: &mut [u64]) -> u64 {
    if successful.len() < 2 {
        return INITIAL_SLOW_SECONDS;
    }
    successful.sort_unstable();
    let mid = successful.len() / 2;
    let median = if successful.len().is_multiple_of(2) {
        let low = successful[mid - 1];
        low + (successful[mid] - low).div_ceil(2)
    } else {
        successful[mid]
    };
    MINIMUM_SLOW_SECONDS.max(median.saturating_mul(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_status_strip_adaptive_slow_threshold_is_bounded_below() {
        assert_eq!(slow_threshold(&mut []), 180);
        assert_eq!(slow_threshold(&mut [10]), 180);
        assert_eq!(slow_threshold(&mut [10, 21]), 60);
        assert_eq!(slow_threshold(&mut [91, 47, 120]), 182);
        assert_eq!(slow_threshold(&mut [u64::MAX - 1, u64::MAX]), u64::MAX);
    }
}
