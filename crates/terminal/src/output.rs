//! Immutable bounded output snapshots for background text processing.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::screen::HistoryCells;
use crate::{Cell, ScreenSnapshot, Terminal};

const HISTORY_ROWS: usize = 256;
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_epoch() -> u64 {
    NEXT_EPOCH.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
pub struct OutputRow {
    cells: HistoryCells,
    wrapped: Option<usize>,
}

impl OutputRow {
    /// Decodes compressed cells on the consuming thread, leaving the terminal actor free.
    pub fn cells(&self) -> Cow<'_, [Cell]> {
        match &self.cells {
            HistoryCells::Unpacked(cells) => Cow::Borrowed(cells),
            HistoryCells::Packed(cells) => Cow::Owned(cells.unpack()),
        }
    }

    pub fn wrapped_columns(&self) -> Option<usize> {
        self.wrapped
    }
}

#[derive(Clone)]
pub struct OutputSnapshot {
    /// Changes when reset, history clearing, or reflow invalidates row coordinates.
    pub epoch: u64,
    /// Absolute row number of the first retained history row.
    pub first_row: usize,
    /// Absolute row number of the live screen's first row.
    pub live_row: usize,
    pub starts_with_continuation: bool,
    pub history: Vec<OutputRow>,
    /// Captured only when the displayed viewport differs from the live screen.
    pub live: Option<ScreenSnapshot>,
}

impl Terminal {
    /// Captures at most 256 immutable history references without decompressing or matching text.
    pub fn output_snapshot(&self, scrolled: bool) -> OutputSnapshot {
        Self::snapshot_output(
            self.model.screen(),
            scrolled.then(|| self.snapshot()),
            HISTORY_ROWS,
        )
    }

    /// Captures a bounded set of primary rows for encrypted session recovery.
    pub fn recovery_output_snapshot(&self, rows: usize) -> Option<OutputSnapshot> {
        if self.model.alternate_screen {
            return None;
        }
        Some(Self::snapshot_output(
            &self.model.primary,
            Some(self.viewport_snapshot(0)),
            rows.min(5000),
        ))
    }

    fn snapshot_output(
        screen: &crate::screen::Screen,
        live: Option<ScreenSnapshot>,
        rows: usize,
    ) -> OutputSnapshot {
        let start = screen.history.len().saturating_sub(rows);
        OutputSnapshot {
            epoch: screen.output_epoch,
            first_row: screen.history_origin.wrapping_add(start),
            live_row: screen.history_origin.wrapping_add(screen.history.len()),
            starts_with_continuation: start > 0 && screen.history[start - 1].wrap_length.is_some(),
            history: screen
                .history
                .iter()
                .skip(start)
                .map(|line| OutputRow {
                    cells: line.cells.clone(),
                    wrapped: line.wrap_length,
                })
                .collect(),
            live,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immutable_rows_survive_trimming_and_reflow_changes_epoch() {
        let mut terminal = Terminal::new(20, 3);
        terminal.advance(b"retained output\r\nnext\r\nlast\r\n");
        let saved = terminal.output_snapshot(false);
        let text: String = saved.history[0]
            .cells()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect();
        for _ in 0..10_400 {
            terminal.advance(b"flood\r\n");
        }
        let after = terminal.output_snapshot(false);
        assert_eq!(after.history.len(), HISTORY_ROWS);
        assert!(after.first_row > saved.live_row);
        assert_eq!(after.epoch, saved.epoch);
        let retained: String = saved.history[0]
            .cells()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect();
        assert_eq!(retained, text);
        terminal.resize(30, 3);
        assert_ne!(terminal.output_snapshot(false).epoch, saved.epoch);
    }

    #[test]
    fn scrolled_capture_retains_the_actual_live_screen() {
        let mut terminal = Terminal::new(20, 2);
        terminal.advance(b"old\r\nolder\r\nactual live");
        let held = terminal.viewport_snapshot(1);
        let output = terminal.output_snapshot(true);
        let live = output.live.unwrap();
        assert_ne!(live.cells, held.cells);
        assert!(
            live.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .contains("actual live")
        );
    }
}
