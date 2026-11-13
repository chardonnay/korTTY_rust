//! Bounded run slots and stable, independently cancellable swarm targets.

use std::collections::HashSet;
use std::fmt::Write as _;

pub(crate) mod comparison;
pub(crate) mod headless;
pub(crate) mod journal;
mod key;
pub(crate) mod report;
pub(crate) mod script;
pub(crate) mod workflow;

pub(crate) const MAX_TARGETS: usize = 8;
pub(crate) const MAX_PARALLEL: usize = 4;
const MAX_DETAIL_CHARS: usize = 512;
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;

pub(crate) use key::Key;

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
}

#[derive(Clone)]
pub(crate) struct Row {
    pub(crate) target: Target,
    pub(crate) status: Status,
    pub(crate) detail: String,
    pub(crate) transcript: String,
    attempt: u64,
    held: bool,
}

impl Row {
    pub(crate) fn attempt(&self) -> u64 {
        self.attempt
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
                attempt: 1,
                held: false,
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
        self.paused = paused;
        if !paused {
            for row in &mut self.rows {
                row.held = false;
            }
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
        let Some(row) = self.row_for(ticket).filter(|row| {
            !row.status.finished() && row.status != Status::Cancelling && row.held != held
        }) else {
            return false;
        };
        row.held = held;
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
        true
    }

    /// Reserves slots before the caller starts workers. Awaiting user review
    /// and cancelling work still own slots until the member's result arrives.
    pub(crate) fn reserve(&mut self) -> Vec<Ticket> {
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

    /// Returns the ticket to cancel only when a worker already owns this row.
    pub(crate) fn cancel(&mut self, key: Key) -> Option<Ticket> {
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
                Some(ticket)
            }
            _ => None,
        }
    }

    fn row_for(&mut self, ticket: Ticket) -> Option<&mut Row> {
        self.current(ticket)?;
        self.rows.get_mut(ticket.index)
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
}
