use std::ops::Range;

use super::super::screenshot_access;
use super::{Journal, PAGE_ROWS, State};

const BASE_THUMBNAIL_HEIGHT: f32 = 112.0;

pub(in crate::workspace::journal_access) struct Layout {
    start: usize,
    offsets: [f32; PAGE_ROWS + 1],
    rows: usize,
}

impl Layout {
    pub(super) fn row_top(&self, ordinal: usize) -> f32 {
        self.offsets[ordinal.saturating_sub(self.start).min(self.rows)]
    }

    pub(super) fn offset(&self, ordinal: usize, fraction: f32) -> f32 {
        let top = self.row_top(ordinal);
        top + fraction * (self.row_top(ordinal + 1) - top)
    }

    pub(super) fn anchor(&self, offset: f32) -> Option<(usize, f32)> {
        let row = self.row_at(offset)?;
        let top = self.offsets[row];
        let height = self.offsets[row + 1] - top;
        Some((self.start + row, ((offset - top) / height).clamp(0.0, 1.0)))
    }

    pub(in crate::workspace::journal_access) fn visible(
        &self,
        offset: f32,
        height: f32,
    ) -> Range<usize> {
        let Some(first) = self.row_at(offset) else {
            return self.start..self.start;
        };
        let last = self.row_at(offset + height).unwrap_or(first);
        (self.start + first)..(self.start + (last + 2).min(self.rows))
    }

    fn row_at(&self, offset: f32) -> Option<usize> {
        (self.rows != 0).then(|| {
            self.offsets[..self.rows]
                .partition_point(|&top| top <= offset)
                .saturating_sub(1)
        })
    }
}

impl State {
    pub(in crate::workspace::journal_access) fn thumbnail_viewport_height(&self) -> f32 {
        self.viewport_height
    }

    pub(in crate::workspace::journal_access) fn thumbnail_height(&self) -> f32 {
        BASE_THUMBNAIL_HEIGHT * f32::from(self.scale) / 100.0
    }

    pub(super) fn row_step(&self) -> f32 {
        self.row_height() + self.row_spacing()
    }

    pub(in crate::workspace::journal_access) fn page_layout(
        &self,
        page: usize,
        document: &Journal,
    ) -> Layout {
        let start = page * PAGE_ROWS;
        let filtered = self.filtered();
        let len = filtered.map_or(document.timeline.len(), <[usize]>::len);
        let rows = len.saturating_sub(start).min(PAGE_ROWS);
        let mut offsets = [0.0; PAGE_ROWS + 1];
        for row in 0..rows {
            let ordinal = start + row;
            let index = filtered.map_or(ordinal, |matches| matches[ordinal]);
            let thumbnail = screenshot_access::reference(&document.timeline[index])
                .map_or(0.0, |_| self.thumbnail_height());
            offsets[row + 1] = offsets[row] + self.row_step() + thumbnail;
        }
        Layout {
            start,
            offsets,
            rows,
        }
    }
}
