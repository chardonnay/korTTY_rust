//! Coordinates shared by a published viewport and later scroll requests.

use std::sync::{Arc, Mutex, RwLock, Weak};

use crate::screen::Position;

/// The history coordinates of a published primary screen.
///
/// Capturing this is constant time. Retained snapshots are remapped when the
/// terminal reflows, so an input event can still address the cells it showed.
#[derive(Clone, Debug)]
pub struct ScrollbackSnapshot(Arc<Snapshot>);

/// The first cell of a held viewport, independent of later output and reflow.
#[derive(Clone, Debug)]
pub struct ScrollbackAnchor {
    snapshot: ScrollbackSnapshot,
    row: usize,
}

impl PartialEq for ScrollbackAnchor {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.snapshot.0, &other.snapshot.0) && self.row == other.row
    }
}

impl Eq for ScrollbackAnchor {}

#[derive(Debug)]
struct Snapshot {
    identity: Arc<RwLock<()>>,
    history: usize,
    rows: usize,
    coordinates: Mutex<Option<Coordinates>>,
}

#[derive(Debug)]
struct Coordinates {
    origin: usize,
    mapped: Option<Box<[Position]>>,
}

impl Coordinates {
    fn position(&self, row: usize) -> Position {
        self.mapped.as_ref().map_or(
            Position {
                row: self.origin.wrapping_add(row),
                column: 0,
            },
            |positions| positions[row],
        )
    }
}

impl ScrollbackSnapshot {
    /// Selects an offset in this snapshot. Zero means following live output.
    pub fn anchor(&self, offset: usize) -> Option<ScrollbackAnchor> {
        (offset != 0 && self.0.history != 0)
            .then(|| self.row_anchor(self.0.history.saturating_sub(offset)))
            .flatten()
    }

    /// Anchors any retained physical row, including the live grid.
    pub fn row_anchor(&self, row: usize) -> Option<ScrollbackAnchor> {
        (row < self.0.rows).then(|| ScrollbackAnchor {
            snapshot: self.clone(),
            row,
        })
    }

    /// Returns an anchor's offset in the layout this snapshot originally showed.
    ///
    /// This is used for successive wheel and keyboard operations before the UI
    /// has received its next frame. Invalidated or unrelated anchors follow live.
    pub fn offset(&self, anchor: Option<&ScrollbackAnchor>) -> usize {
        anchor
            .and_then(|anchor| self.row(anchor))
            .map_or(0, |index| self.0.history.saturating_sub(index))
    }

    /// Resolves a physical anchor in the layout this snapshot showed.
    pub fn row(&self, anchor: &ScrollbackAnchor) -> Option<usize> {
        if !Arc::ptr_eq(&self.0.identity, &anchor.snapshot.0.identity) {
            return None;
        }
        let _layout = self.0.identity.read().unwrap_or_else(|e| e.into_inner());
        if Arc::ptr_eq(&self.0, &anchor.snapshot.0) {
            return if self
                .0
                .coordinates
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
            {
                Some(anchor.row)
            } else {
                None
            };
        }
        let position = anchor.position()?;
        let coordinates = self.0.coordinates.lock().unwrap_or_else(|e| e.into_inner());
        let coordinates = coordinates.as_ref()?;
        let index = if let Some(positions) = &coordinates.mapped {
            positions
                .partition_point(|candidate| {
                    candidate.row < position.row
                        || (candidate.row == position.row && candidate.column <= position.column)
                })
                .saturating_sub(1)
        } else {
            retained_index(position.row, coordinates.origin, self.0.rows - 1)
        };
        Some(index)
    }
}

impl ScrollbackAnchor {
    fn position(&self) -> Option<Position> {
        self.snapshot
            .0
            .coordinates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|coordinates| coordinates.position(self.row))
    }
}

/// The bounded history is much shorter than half the ordinal address space.
fn retained_index(row: usize, origin: usize, last: usize) -> usize {
    let distance = row.wrapping_sub(origin);
    if distance > isize::MAX as usize {
        0
    } else {
        distance.min(last)
    }
}

#[derive(Default)]
pub(crate) struct ScrollbackRegistry {
    identity: Arc<RwLock<()>>,
    snapshots: Mutex<Vec<Weak<Snapshot>>>,
}

impl ScrollbackRegistry {
    pub(crate) fn snapshot(
        &self,
        origin: usize,
        history: usize,
        rows: usize,
    ) -> ScrollbackSnapshot {
        let snapshot = Arc::new(Snapshot {
            identity: Arc::clone(&self.identity),
            history,
            rows: history + rows,
            coordinates: Mutex::new(Some(Coordinates {
                origin,
                mapped: None,
            })),
        });
        let mut snapshots = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        snapshots.retain(|snapshot| snapshot.strong_count() != 0);
        snapshots.push(Arc::downgrade(&snapshot));
        ScrollbackSnapshot(snapshot)
    }

    pub(crate) fn offset(&self, origin: usize, history: usize, anchor: &ScrollbackAnchor) -> usize {
        if !Arc::ptr_eq(&self.identity, &anchor.snapshot.0.identity) {
            return 0;
        }
        let _layout = self.identity.read().unwrap_or_else(|e| e.into_inner());
        anchor.position().map_or(0, |position| {
            history - retained_index(position.row, origin, history)
        })
    }

    pub(crate) fn has_snapshots(&mut self) -> bool {
        let snapshots = self.snapshots.get_mut().unwrap_or_else(|e| e.into_inner());
        snapshots.retain(|snapshot| snapshot.strong_count() != 0);
        !snapshots.is_empty()
    }

    pub(crate) fn invalidate(&mut self) {
        let _layout = self.identity.write().unwrap_or_else(|e| e.into_inner());
        for snapshot in self
            .snapshots
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            if let Some(snapshot) = snapshot.upgrade() {
                *snapshot
                    .coordinates
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    }

    pub(crate) fn reflow(&mut self, mapping: &ReflowMap) {
        // A wheel request can consult two snapshots. Publish every remapped
        // snapshot under one layout lock so it cannot mix coordinate systems.
        let _layout = self.identity.write().unwrap_or_else(|e| e.into_inner());
        let snapshots = self.snapshots.get_mut().unwrap_or_else(|e| e.into_inner());
        snapshots.retain(|snapshot| {
            let Some(snapshot) = snapshot.upgrade() else {
                return false;
            };
            let mut coordinates = snapshot
                .coordinates
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(coordinates) = coordinates.as_mut() {
                let mapped = (0..snapshot.rows)
                    .map(|row| mapping.position(coordinates.position(row)))
                    .collect();
                coordinates.mapped = Some(mapped);
            }
            true
        });
    }
}

impl Drop for ScrollbackRegistry {
    fn drop(&mut self) {
        self.invalidate();
    }
}

/// A temporary piecewise-linear map built by the existing reflow walk. Only
/// live snapshots retain its results, one position per original physical row.
pub(crate) struct ReflowMap {
    origin: usize,
    rows: Vec<usize>,
    runs: Vec<MappingRun>,
}

struct MappingRun {
    column: usize,
    length: usize,
    target: Position,
}

impl ReflowMap {
    pub(crate) fn new(origin: usize) -> Self {
        Self {
            origin,
            rows: Vec::new(),
            runs: Vec::new(),
        }
    }

    pub(crate) fn record(&mut self, row: usize, column: usize, target: Position) {
        if self.rows.len() == row {
            self.rows.push(self.runs.len());
        } else if let Some(previous) = self.runs.last_mut()
            && previous.column + previous.length == column
            && previous.target.row == target.row
            && previous.target.column + previous.length == target.column
        {
            previous.length += 1;
            return;
        }
        self.runs.push(MappingRun {
            column,
            length: 1,
            target,
        });
    }

    fn position(&self, source: Position) -> Position {
        let evicted = source.row.wrapping_sub(self.origin) > isize::MAX as usize;
        let row = retained_index(source.row, self.origin, self.rows.len() - 1);
        let start = self.rows[row];
        let end = self.rows.get(row + 1).copied().unwrap_or(self.runs.len());
        let runs = &self.runs[start..end];
        let column = if evicted { 0 } else { source.column };
        let index = runs
            .partition_point(|run| run.column <= column)
            .saturating_sub(1);
        let run = &runs[index];
        Position {
            row: run.target.row,
            column: run.target.column + column.saturating_sub(run.column).min(run.length - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScreenSnapshot, Terminal};

    fn first_row(screen: &ScreenSnapshot) -> String {
        screen
            .row(0)
            .unwrap()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn held_rows_survive_append_and_both_history_budgets() {
        for (columns, initial, extra) in [(16, 10_050, 100), (1024, 1000, 20)] {
            let mut terminal = Terminal::new(columns, 3);
            for index in 0..initial {
                terminal.advance(format!("row-{index}\r\n").as_bytes());
            }
            let before = terminal.snapshot().scrollback_lines;
            if columns == 16 {
                assert_eq!(before, 10_000);
            } else {
                assert!(
                    before < 1000,
                    "the byte budget must evict before the row budget"
                );
            }
            let snapshot = terminal.scrollback_snapshot();
            let anchor = snapshot.anchor(before / 2).unwrap();
            let oldest = snapshot.anchor(before).unwrap();
            let expected = first_row(&terminal.viewport_snapshot(before / 2));
            for index in 0..extra {
                terminal.advance(format!("new-{index}\r\n").as_bytes());
            }
            let offset = terminal.scrollback_offset(Some(&anchor));
            assert_eq!(offset, before / 2 + extra);
            assert_eq!(first_row(&terminal.viewport_snapshot(offset)), expected);
            assert_eq!(terminal.scrollback_offset(Some(&oldest)), before);
            assert_eq!(terminal.scrollback_offset(None), 0);
            while terminal.compress_idle_scrollback(offset) {}
            assert_eq!(first_row(&terminal.viewport_snapshot(offset)), expected);
        }
    }

    #[test]
    fn retained_frame_coordinates_preserve_wide_cells_across_repeated_reflow() {
        let mut terminal = Terminal::new(8, 3);
        terminal.advance("1234567界abcdefghi\r\nsecond\r\nthird\r\nfourth\r\nlast".as_bytes());
        let hit = terminal.search("界abc", false).matches[0].start;
        assert_eq!(hit.column, 0);
        let history = terminal.snapshot().scrollback_lines;
        let stale = terminal.scrollback_snapshot();
        let anchor = stale.anchor(history - hit.row).unwrap();
        let source_rows = stale.0.rows;
        for _ in 0..4 {
            for columns in [17, 5, 9, 4, 8] {
                terminal.resize(columns, 3);
                let hit = terminal.search("界abc", false).matches[0].start;
                let offset = terminal.scrollback_offset(Some(&anchor));
                assert_eq!(offset, terminal.snapshot().scrollback_lines - hit.row);
                let position = anchor.position().unwrap();
                assert_eq!(position.column, usize::from(hit.column));
                assert_eq!(
                    stale
                        .0
                        .coordinates
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .mapped
                        .as_ref()
                        .unwrap()
                        .len(),
                    source_rows,
                    "each retained snapshot keeps one composed map"
                );
                let fresh = terminal.scrollback_snapshot();
                assert_eq!(fresh.offset(Some(&anchor)), offset);
            }
        }
        assert_eq!(anchor.position().unwrap().column, 0);
        assert!(
            first_row(&terminal.viewport_snapshot(terminal.scrollback_offset(Some(&anchor))))
                .starts_with("界abc")
        );
        // A delayed thumb event from the original frame addresses the same cell.
        let delayed = stale.anchor(history - hit.row).unwrap();
        assert_eq!(
            terminal.scrollback_offset(Some(&delayed)),
            terminal.scrollback_offset(Some(&anchor))
        );
    }

    #[test]
    fn alternate_screen_and_row_only_resize_retain_primary_anchor() {
        let mut terminal = Terminal::new(16, 3);
        terminal.advance(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        let snapshot = terminal.scrollback_snapshot();
        let anchor = snapshot.anchor(2).unwrap();
        let expected = first_row(&terminal.viewport_snapshot(2));
        terminal.advance(b"\x1b[?1049hALTERNATE\r\nTEMPORARY");
        assert_eq!(terminal.scrollback_offset(Some(&anchor)), 0);
        assert_eq!(terminal.primary_scrollback_offset(Some(&anchor)), 2);
        terminal.resize(10, 5);
        terminal.resize(16, 3);
        terminal.advance(b"\x1b[?1049l");
        let offset = terminal.scrollback_offset(Some(&anchor));
        assert_eq!(first_row(&terminal.viewport_snapshot(offset)), expected);
        terminal.resize(16, 5);
        terminal.resize(16, 3);
        let offset = terminal.scrollback_offset(Some(&anchor));
        assert_eq!(first_row(&terminal.viewport_snapshot(offset)), expected);
    }

    #[test]
    fn clear_reset_and_reconnect_reject_stale_coordinates() {
        for reset in [b"\x1b[3J".as_slice(), b"\x1bc".as_slice()] {
            let mut terminal = Terminal::new(12, 2);
            terminal.advance(b"old-one\r\nold-two\r\nold-three\r\nlive");
            let snapshot = terminal.scrollback_snapshot();
            let anchor = snapshot.anchor(1).unwrap();
            terminal.advance(reset);
            terminal.advance(b"new-one\r\nnew-two\r\nnew-three\r\nnew-live");
            assert!(terminal.snapshot().scrollback_lines > 0);
            assert_eq!(terminal.scrollback_offset(Some(&anchor)), 0);
            assert_eq!(snapshot.offset(Some(&anchor)), 0);
        }
        let mut terminal = Terminal::new(12, 2);
        terminal.advance(b"retained\r\nsecond\r\nlive");
        let snapshot = terminal.scrollback_snapshot();
        let anchor = snapshot.anchor(1).unwrap();
        terminal.advance(b"\x1b[!p");
        assert_eq!(terminal.scrollback_offset(Some(&anchor)), 1);
        let imported = terminal.reconnect_history();
        terminal.prepend_reconnect_history(imported);
        assert_eq!(terminal.scrollback_offset(Some(&anchor)), 0);
        let mut replacement = Terminal::new(12, 2);
        replacement.advance(b"other\r\nrows\r\nlive");
        assert_eq!(replacement.scrollback_offset(Some(&anchor)), 0);
    }

    #[test]
    fn publishing_many_frames_prunes_expired_coordinate_registrations() {
        let registry = ScrollbackRegistry::default();
        let retained = registry.snapshot(100, 20, 3);
        for _ in 0..20_000 {
            drop(registry.snapshot(100, 20, 3));
        }
        let snapshots = registry.snapshots.lock().unwrap();
        assert!(snapshots.len() <= 2);
        assert!(snapshots.capacity() <= 4);
        assert_eq!(retained.offset(retained.anchor(5).as_ref()), 5);
    }
}
