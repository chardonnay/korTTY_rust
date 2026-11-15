//! Bounded run slots and stable, independently cancellable swarm targets.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Instant;

pub(crate) mod comparison;
pub(crate) mod headless;
pub(crate) mod journal;
mod key;
pub(crate) mod report;
pub(crate) mod script;
mod status_strip;
pub(crate) mod workflow;

pub(crate) const MAX_TARGETS: usize = 8;
pub(crate) const MAX_PARALLEL: usize = 4;
const MAX_DETAIL_CHARS: usize = 512;
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;

pub(crate) use key::Key;
pub(crate) use status_strip::StatusCounts;
use status_strip::{Clock, StatusStrip};

#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) key: Key,
    pub(crate) label: String,
    pub(crate) private: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ticket {
    round: u64,
    index: usize,
    key: Key,
    attempt: u64,
}

impl Ticket {
    pub(crate) fn key(self) -> Key {
        self.key
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Status {
    Queued,
    Starting,
    Running,
    Review,
    Paused,
    Cancelling,
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
}

impl Status {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Review => "Awaiting review",
            Self::Paused => "Paused",
            Self::Cancelling => "Cancelling",
            Self::Succeeded => "Completed",
            Self::Failed => "Failed",
            Self::Blocked => "Blocked",
            Self::Cancelled => "Cancelled",
        }
    }

    pub(crate) fn finished(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Blocked | Self::Cancelled
        )
    }

    fn holds_slot(self) -> bool {
        self != Self::Queued && !self.finished()
    }

    fn counts_elapsed(self) -> bool {
        self.holds_slot() && !matches!(self, Self::Paused | Self::Cancelling)
    }

    fn is_working(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Clone)]
pub(crate) struct Row {
    pub(crate) target: Target,
    pub(crate) status: Status,
    pub(crate) detail: String,
    pub(crate) transcript: String,
    pub(crate) usage_tokens: Option<u64>,
    attempt: u64,
    held: bool,
    clock: Option<Clock>,
}

impl Row {
    pub(crate) fn attempt(&self) -> u64 {
        self.attempt
    }

    pub(crate) fn elapsed_seconds(&self, now: Instant) -> Option<u64> {
        self.clock.as_ref().map(|clock| clock.elapsed_seconds(now))
    }

    fn sync_clock(&mut self, now: Instant, round_paused: bool) {
        if let Some(clock) = &mut self.clock {
            clock.set_running(
                now,
                !self.held && !round_paused && self.status.counts_elapsed(),
            );
        }
    }

    fn display_status(&self, round_paused: bool) -> Status {
        if (self.held || (round_paused && self.status.holds_slot()))
            && self.status != Status::Cancelling
            && !self.status.finished()
        {
            Status::Paused
        } else {
            self.status
        }
    }
}

pub(crate) struct Queue {
    round: u64,
    parallel: usize,
    paused: bool,
    rows: Vec<Row>,
}

impl Queue {
    pub(crate) fn new(
        round: u64,
        targets: Vec<Target>,
        parallel: usize,
    ) -> Result<Self, &'static str> {
        let mut identities = HashSet::new();
        if round == 0
            || targets.is_empty()
            || targets.len() > MAX_TARGETS
            || !(1..=MAX_PARALLEL).contains(&parallel)
            || targets.iter().any(|target| {
                !identities.insert(target.key.stable()) || target.label.trim().is_empty()
            })
        {
            return Err("Choose one to eight distinct targets and up to four active runs.");
        }
        let rows = targets
            .into_iter()
            .map(|target| Row {
                target,
                status: Status::Queued,
                detail: String::new(),
                transcript: String::new(),
                usage_tokens: None,
                attempt: 1,
                held: false,
                clock: None,
            })
            .collect();
        Ok(Self {
            round,
            parallel,
            paused: false,
            rows,
        })
    }

    pub(crate) fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub(crate) fn complete(&self) -> bool {
        self.rows.iter().all(|row| row.status.finished())
    }

    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.set_paused_at(paused, Instant::now());
    }

    fn set_paused_at(&mut self, paused: bool, now: Instant) {
        self.paused = paused;
        for row in &mut self.rows {
            if !paused {
                row.held = false;
            }
            row.sync_clock(now, paused);
        }
    }

    pub(crate) fn paused(&self) -> bool {
        self.paused
    }

    pub(crate) fn ticket(&self, key: Key) -> Option<Ticket> {
        self.rows.iter().enumerate().find_map(|(index, row)| {
            (row.target.key == key).then_some(Ticket {
                round: self.round,
                index,
                key,
                attempt: row.attempt,
            })
        })
    }

    pub(crate) fn current(&self, ticket: Ticket) -> Option<&Row> {
        if ticket.round != self.round {
            return None;
        }
        self.rows
            .get(ticket.index)
            .filter(|row| row.target.key == ticket.key && row.attempt == ticket.attempt)
    }

    pub(crate) fn held(&self, ticket: Ticket) -> bool {
        self.current(ticket).is_some_and(|row| row.held)
    }

    pub(crate) fn hold(&mut self, ticket: Ticket, held: bool) -> bool {
        self.hold_at(ticket, held, Instant::now())
    }

    fn hold_at(&mut self, ticket: Ticket, held: bool, now: Instant) -> bool {
        let paused = self.paused;
        let Some(row) = self.row_for(ticket).filter(|row| {
            !row.status.finished() && row.status != Status::Cancelling && row.held != held
        }) else {
            return false;
        };
        row.held = held;
        row.sync_clock(now, paused);
        true
    }

    pub(crate) fn retry(&mut self, ticket: Ticket) -> bool {
        let Some(row) = self.row_for(ticket).filter(|row| row.status.finished()) else {
            return false;
        };
        let Some(attempt) = row.attempt.checked_add(1) else {
            return false;
        };
        row.attempt = attempt;
        row.held = false;
        row.status = Status::Queued;
        row.detail.clear();
        row.transcript.clear();
        row.usage_tokens = None;
        row.clock = None;
        true
    }

    /// Reserves slots before the caller starts workers. Awaiting user review
    /// and cancelling work still own slots until the member's result arrives.
    pub(crate) fn reserve(&mut self) -> Vec<Ticket> {
        self.reserve_at(Instant::now())
    }

    fn reserve_at(&mut self, now: Instant) -> Vec<Ticket> {
        if self.paused {
            return Vec::new();
        }
        let occupied = self
            .rows
            .iter()
            .filter(|row| row.status.holds_slot())
            .count();
        self.rows
            .iter_mut()
            .enumerate()
            .filter(|(_, row)| row.status == Status::Queued && !row.held)
            .take(self.parallel.saturating_sub(occupied))
            .map(|(index, row)| {
                row.status = Status::Starting;
                row.clock = Some(Clock::start(now));
                Ticket {
                    round: self.round,
                    index,
                    key: row.target.key,
                    attempt: row.attempt,
                }
            })
            .collect()
    }

    pub(crate) fn change(
        &mut self,
        ticket: Ticket,
        status: Status,
        detail: &str,
        transcript: &str,
    ) -> bool {
        self.change_at(ticket, status, detail, transcript, Instant::now())
    }

    fn change_at(
        &mut self,
        ticket: Ticket,
        status: Status,
        detail: &str,
        transcript: &str,
        now: Instant,
    ) -> bool {
        let paused = self.paused;
        let Some(row) = self.row_for(ticket) else {
            return false;
        };
        if !row.status.holds_slot()
            || status == Status::Queued
            || (row.status == Status::Cancelling && !status.finished())
        {
            return false;
        }
        row.status = status;
        row.sync_clock(now, paused);
        row.detail = detail.chars().take(MAX_DETAIL_CHARS).collect();
        let cut =
            transcript.ceil_char_boundary(transcript.len().saturating_sub(MAX_TRANSCRIPT_BYTES));
        let tail = &transcript[cut..];
        row.transcript = if cut == 0 {
            tail.into()
        } else {
            let complete_line = tail.find('\n').map_or("", |index| &tail[index + 1..]);
            format!("Earlier transcript omitted.\n{complete_line}")
        };
        true
    }

    pub(crate) fn set_label(&mut self, ticket: Ticket, label: String) {
        if let Some(row) = self.row_for(ticket).filter(|row| row.status.holds_slot()) {
            row.target.label = label;
        }
    }

    pub(crate) fn set_usage_tokens(&mut self, ticket: Ticket, tokens: Option<u64>) {
        if let Some(row) = self.row_for(ticket) {
            row.usage_tokens = tokens;
        }
    }

    /// Returns the ticket to cancel only when a worker already owns this row.
    pub(crate) fn cancel(&mut self, key: Key) -> Option<Ticket> {
        self.cancel_at(key, Instant::now())
    }

    fn cancel_at(&mut self, key: Key, now: Instant) -> Option<Ticket> {
        let ticket = self.ticket(key)?;
        let row = self.row_for(ticket)?;
        match row.status {
            Status::Queued => {
                row.status = Status::Cancelled;
                row.detail = "Cancelled before starting".into();
                None
            }
            Status::Cancelling => None,
            status if status.holds_slot() => {
                row.status = Status::Cancelling;
                row.sync_clock(now, false);
                Some(ticket)
            }
            _ => None,
        }
    }

    fn row_for(&mut self, ticket: Ticket) -> Option<&mut Row> {
        self.current(ticket)?;
        self.rows.get_mut(ticket.index)
    }

    pub(crate) fn status_strip(&self, now: Instant) -> StatusStrip<'_> {
        StatusStrip::new(self, now)
    }

    pub(crate) fn has_clock(&self) -> bool {
        self.rows
            .iter()
            .any(|row| row.clock.as_ref().is_some_and(Clock::is_running))
    }

    pub(crate) fn has_animated_target(&self) -> bool {
        self.rows.iter().any(|row| {
            let status = row.display_status(self.paused);
            status.is_working() || status == Status::Review
        })
    }

    pub(crate) fn report(&self) -> String {
        let mut output = String::from("| Target | Status | Result |\n| --- | --- | --- |\n");
        for row in &self.rows {
            let _ = writeln!(
                output,
                "| {} | {} | {} |",
                cell(&row.target.label),
                row.status.label(),
                cell(&row.detail)
            );
        }
        output
    }
}

fn cell(value: &str) -> String {
    value
        .chars()
        .take(MAX_DETAIL_CHARS)
        .flat_map(|character| match character {
            '\r' | '\n' => [' ', '\0'],
            '|' | '\\' | '<' | '>' | '[' | ']' => ['\\', character],
            _ => [character, '\0'],
        })
        .filter(|&character| character != '\0')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

    fn targets() -> Vec<Target> {
        (1..=4)
            .map(|index| Target {
                key: Key::Live {
                    session: SessionId(index),
                    stream: 1,
                },
                label: format!("Target {index}"),
                private: false,
            })
            .collect()
    }

    #[test]
    fn paused_queued_targets_release_slots_and_retry_retires_the_old_ticket() {
        let mut queue = Queue::new(1, targets(), 1).unwrap();
        let first = queue.ticket(queue.rows()[0].target.key).unwrap();
        assert!(queue.hold(first, true));
        assert!(queue.held(first));
        let second = queue.reserve()[0];
        assert_ne!(second, first);
        assert_eq!(queue.cancel(second.key()), Some(second));
        assert!(!queue.retry(second));
        assert!(queue.change(second, Status::Cancelled, "old attempt", "old transcript"));
        assert!(queue.retry(second));
        assert!(queue.current(second).is_none());
        let restarted = queue.reserve()[0];
        assert_eq!(restarted.key(), second.key());
        assert_ne!(restarted, second);
        assert!(queue.current(restarted).unwrap().transcript.is_empty());
        assert!(!queue.change(second, Status::Failed, "late callback", ""));
        queue.set_paused(true);
        assert!(queue.reserve().is_empty());
        queue.set_paused(false);
        assert!(!queue.held(first));
    }

    #[test]
    fn swarm_queue_keeps_review_slots_and_isolates_cancellation_and_failure() {
        let mut queue = Queue::new(1, targets(), 2).unwrap();
        let pair = queue.reserve();
        assert_eq!(pair.len(), 2);
        assert!(queue.change(pair[0], Status::Review, "Review", ""));
        assert!(queue.reserve().is_empty());
        assert_eq!(queue.cancel(pair[0].key()), Some(pair[0]));
        assert!(!queue.change(pair[0], Status::Running, "Stale", ""));
        assert!(queue.reserve().is_empty());
        assert!(queue.change(pair[1], Status::Failed, "Remote failure", "Partial output"));
        let next = queue.reserve();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].key().pane().unwrap().0, SessionId(3));
        assert!(queue.change(pair[0], Status::Cancelled, "Cancelled", ""));
        assert_eq!(queue.reserve().len(), 1);
        assert_eq!(queue.rows()[1].transcript, "Partial output");
        assert!(queue.report().contains("Remote failure"));
        assert!(!queue.complete());
    }

    #[test]
    fn swarm_queue_rejects_duplicate_panes_and_stale_round_tickets() {
        let mut duplicate = targets();
        let (session, stream) = duplicate[0].key.pane().unwrap();
        duplicate[1].key = Key::Live {
            session,
            stream: stream + 1,
        };
        assert!(Queue::new(1, duplicate, 4).is_err());
        let mut first = Queue::new(1, targets(), 2).unwrap();
        let obsolete = first.reserve()[0];
        let mut second = Queue::new(2, targets(), 2).unwrap();
        second.set_paused(true);
        assert!(second.reserve().is_empty());
        second.set_paused(false);
        let current = second.reserve()[0];
        assert!(!second.change(obsolete, Status::Succeeded, "Stale", ""));
        assert!(second.change(current, Status::Succeeded, "OK", ""));
    }

    #[test]
    fn swarm_saved_keys_do_not_alias_live_panes_with_the_same_number() {
        let mut selected = targets();
        let (SessionId(id), _) = selected[0].key.pane().unwrap();
        selected[1].key = Key::Saved(id);
        assert!(Queue::new(1, selected.clone(), 2).is_ok());
        selected[2].key = Key::Saved(id);
        assert!(Queue::new(1, selected, 2).is_err());
    }

    #[test]
    fn swarm_status_strip_uses_successful_elapsed_and_excludes_review_from_slow() {
        use std::time::Duration;

        let mut queue = Queue::new(1, targets(), 4).unwrap();
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let tickets = queue.reserve_at(start);
        assert!(queue.change_at(tickets[2], Status::Failed, "Failed", "", at(12)));
        assert!(queue.change_at(tickets[0], Status::Succeeded, "Done", "", at(91)));
        assert!(queue.change_at(tickets[3], Status::Review, "Review", "", at(100)));
        let before = queue.status_strip(at(179));
        assert!(!before.targets[1].slow);
        assert_eq!(before.targets[3].elapsed_seconds, Some(179));
        assert!(!before.targets[3].slow);
        assert!(queue.status_strip(at(180)).targets[1].slow);
        assert!(queue.change_at(tickets[3], Status::Succeeded, "Done", "", at(200)));
        // Ceil((91 + 200) / 2) * 2 = 292; the failed 12-second attempt is excluded.
        let before_threshold = queue.status_strip(at(291));
        assert!(!before_threshold.targets[1].slow);
        let strip = queue.status_strip(at(292));
        assert_eq!(strip.targets[1].ticket, tickets[1]);
        assert_eq!(strip.targets[1].row.target.label, "Target 2");
        assert_eq!(strip.targets[1].elapsed_seconds, Some(292));
        assert!(strip.targets[1].slow);
        assert_eq!(strip.targets[0].elapsed_seconds, Some(91));
        assert_eq!(strip.targets[2].elapsed_seconds, Some(12));
    }

    #[test]
    fn swarm_status_strip_freezes_pauses_cancellation_and_resets_each_attempt() {
        use std::time::Duration;

        let mut queue = Queue::new(1, targets(), 1).unwrap();
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let ticket = queue.reserve_at(start)[0];
        assert!(queue.has_clock());
        assert!(queue.has_animated_target());
        let held = queue.ticket(queue.rows()[1].target.key).unwrap();
        assert!(queue.hold_at(held, true, at(2)));
        let strip = queue.status_strip(at(3));
        assert_eq!(strip.targets[1].state, Status::Paused);
        assert_eq!(strip.targets[1].elapsed_seconds, None);
        assert!(queue.hold_at(ticket, true, at(5)));
        assert_eq!(
            queue.status_strip(at(40)).targets[0].elapsed_seconds,
            Some(5)
        );
        assert!(!queue.has_clock());
        assert!(!queue.has_animated_target());
        queue.set_paused_at(true, at(45));
        queue.set_paused_at(false, at(50));
        assert!(queue.has_clock());
        assert!(!queue.held(held));
        assert!(queue.change_at(ticket, Status::Review, "Review", "", at(55)));
        assert!(queue.has_clock());
        assert!(queue.has_animated_target());
        assert_eq!(
            queue.status_strip(at(65)).targets[0].elapsed_seconds,
            Some(20)
        );
        queue.set_usage_tokens(ticket, Some(42));
        assert_eq!(queue.current(ticket).unwrap().usage_tokens, Some(42));
        assert_eq!(queue.cancel_at(ticket.key(), at(70)), Some(ticket));
        assert!(!queue.has_clock());
        assert!(queue.change_at(ticket, Status::Cancelled, "Cancelled", "", at(90)));
        assert_eq!(
            queue.status_strip(at(100)).targets[0].elapsed_seconds,
            Some(25)
        );
        assert!(queue.retry(ticket));
        let retried = queue.reserve_at(at(100))[0];
        queue.set_usage_tokens(ticket, Some(99));
        let row = queue.current(retried).unwrap();
        assert_eq!(row.elapsed_seconds(at(102)), Some(2));
        assert_eq!(row.usage_tokens, None);
        queue.set_usage_tokens(retried, Some(3));
        queue.set_usage_tokens(retried, None);
        assert_eq!(queue.current(retried).unwrap().usage_tokens, None);
    }
}
