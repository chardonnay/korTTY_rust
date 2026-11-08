use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{
    Cell, KittyPlacement, ScrollbackAnchor, ScrollbackSnapshot, Style, ViewportImage,
    command_output::CommandMarks,
    kitty::MAX_PLACEMENTS_PER_LINE,
    packed_cells::PackedCells,
    prompts::{LineMark, LineMarkKind, PromptLayout},
    rectangle::Rectangle,
    scrollback::{ReflowMap, ScrollbackRegistry},
};

const MAX_HISTORY_LINES: usize = 10_000;
const MAX_HISTORY_BYTES: usize = 10 * 1024 * 1024;
const PACK_SCAN_ROWS: usize = 64;
const PACK_SLICE: Duration = Duration::from_millis(4);
const MAX_MARKS_PER_KIND_PER_LINE: usize = 8;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Position {
    pub(crate) row: usize,
    pub(crate) column: usize,
}

pub(crate) struct Line {
    pub(crate) cells: Vec<Cell>,
    pub(crate) images: Vec<KittyPlacement>,
    pub(crate) wrap_length: Option<usize>,
    pub(crate) hp_form: Option<Box<crate::hp::FormLine>>,
    pub(crate) marks: Vec<LineMark>,
    // Reflow preserves this boundary even when an archived row wraps.
    archived: bool,
}

impl Line {
    fn blank(columns: usize, style: Style) -> Self {
        Self {
            cells: vec![Cell::blank(style); columns],
            images: Vec::new(),
            wrap_length: None,
            hp_form: None,
            marks: Vec::new(),
            archived: false,
        }
    }

    fn is_empty_for_history(&self) -> bool {
        self.images.is_empty()
            && self.marks.is_empty()
            && self.cells.iter().all(Cell::is_default_blank)
    }

    pub(crate) fn add_mark(&mut self, mark: LineMark) {
        if mark.kind.is_timestamp() {
            let mut timestamps = self
                .marks
                .iter_mut()
                .filter(|mark| mark.kind.is_timestamp());
            let mut count = 0;
            for existing in &mut timestamps {
                if existing.column == mark.column {
                    if mark.id < existing.id {
                        *existing = mark;
                    }
                    return;
                }
                count += 1;
            }
            if count < MAX_MARKS_PER_KIND_PER_LINE {
                self.marks.push(mark);
            }
            return;
        }
        if let Some(existing) = self
            .marks
            .iter_mut()
            .find(|existing| existing.kind == mark.kind && existing.column == mark.column)
        {
            if mark.id > existing.id {
                *existing = mark;
            }
            return;
        }
        let same_kind = |existing: &&mut LineMark| existing.kind == mark.kind;
        if self.marks.iter_mut().filter(same_kind).count() == MAX_MARKS_PER_KIND_PER_LINE {
            if let Some(oldest) = self
                .marks
                .iter_mut()
                .filter(same_kind)
                .min_by_key(|mark| mark.id)
                && oldest.id < mark.id
            {
                *oldest = mark;
            }
            return;
        }
        self.marks.push(mark);
    }

    fn has_prompt(&self) -> bool {
        self.marks
            .iter()
            .any(|mark| mark.kind == LineMarkKind::Prompt)
    }

    fn shift_marks(&mut self, left: usize, right: usize, delta: isize) {
        self.marks.retain_mut(|mark| {
            if !(left..=right).contains(&mark.column) {
                return true;
            }
            if let Some(column) = mark.column.checked_add_signed(delta)
                && (left..=right).contains(&column)
            {
                mark.column = column;
                true
            } else {
                false
            }
        });
    }

    fn move_mark_columns(&mut self, source: &mut Self, left: usize, right: usize) {
        self.marks
            .retain(|mark| !(left..=right).contains(&mark.column));
        for mark in source
            .marks
            .extract_if(.., |mark| (left..=right).contains(&mark.column))
        {
            self.add_mark(mark);
        }
    }

    fn history_bytes(&self) -> usize {
        self.cells.capacity() * size_of::<Cell>()
            + self
                .cells
                .iter()
                .map(|cell| {
                    // Counting a shared link per cell overestimates the cost and keeps the bound safe.
                    cell.text.capacity() + cell.hyperlink.as_ref().map_or(0, |link| link.len())
                })
                .sum::<usize>()
    }

    fn clear_glyph(&mut self, column: usize, blank: &Cell) {
        if self.cells[column].width == 0 && column > 0 {
            self.cells[column - 1] = blank.clone();
        }
        if self.cells[column].width == 2 && column + 1 < self.cells.len() {
            self.cells[column + 1] = blank.clone();
        }
        self.cells[column] = blank.clone();
    }

    pub(crate) fn glyph_bounds(&self, left: usize, right: usize) -> (usize, usize) {
        let left = if self.cells[left].width == 0 {
            left.saturating_sub(1)
        } else {
            left
        };
        let right = if self.cells[right].width == 2 {
            (right + 1).min(self.cells.len() - 1)
        } else {
            right
        };
        (left, right)
    }

    fn repair_wide_cells(&mut self, blank: &Cell) {
        let columns = self.cells.len();
        for column in 0..columns {
            match self.cells[column].width {
                0 if column == 0 || self.cells[column - 1].width != 2 => {
                    self.cells[column] = blank.clone();
                }
                2 if column + 1 == columns || self.cells[column + 1].width != 0 => {
                    self.cells[column] = blank.clone();
                }
                _ => {}
            }
        }
    }
}

#[derive(Clone)]
enum HistoryCells {
    Unpacked(Vec<Cell>),
    Packed(PackedCells),
}

#[derive(Clone)]
pub(crate) struct HistoryLine {
    cells: HistoryCells,
    pub(crate) images: Vec<KittyPlacement>,
    pub(crate) wrap_length: Option<usize>,
    bytes: usize,
    pub(crate) marks: Vec<LineMark>,
    archived: bool,
}

impl HistoryLine {
    fn new(line: Line) -> Self {
        let bytes = line.history_bytes();
        Self {
            cells: HistoryCells::Unpacked(line.cells),
            images: line.images,
            wrap_length: line.wrap_length,
            bytes,
            marks: line.marks,
            archived: line.archived,
        }
    }

    pub(crate) fn filter_hyperlinks(&mut self, filter: impl Fn(&mut Option<Arc<str>>)) {
        match &mut self.cells {
            HistoryCells::Unpacked(cells) => {
                for cell in cells {
                    filter(&mut cell.hyperlink);
                }
            }
            HistoryCells::Packed(cells) => cells.filter_hyperlinks(filter),
        }
    }

    pub(crate) fn cells(&self) -> Cow<'_, [Cell]> {
        match &self.cells {
            HistoryCells::Unpacked(cells) => Cow::Borrowed(cells),
            HistoryCells::Packed(cells) => Cow::Owned(cells.unpack()),
        }
    }

    fn has_prompt(&self) -> bool {
        self.marks
            .iter()
            .any(|mark| mark.kind == LineMarkKind::Prompt)
    }

    fn has_timestamp(&self) -> bool {
        self.marks.iter().any(|mark| mark.kind.is_timestamp())
    }

    fn pack(&mut self) -> usize {
        if let HistoryCells::Unpacked(cells) = &self.cells
            && let Some((packed, bytes)) = PackedCells::pack(cells, self.bytes)
        {
            self.cells = HistoryCells::Packed(packed);
            return std::mem::replace(&mut self.bytes, bytes) - bytes;
        }
        0
    }

    fn into_line(self) -> Line {
        Line {
            cells: match self.cells {
                HistoryCells::Unpacked(cells) => cells,
                HistoryCells::Packed(cells) => cells.unpack(),
            },
            images: self.images,
            wrap_length: self.wrap_length,
            hp_form: None,
            marks: self.marks,
            archived: self.archived,
        }
    }
}

pub(crate) struct Screen {
    pub(crate) lines: VecDeque<Line>,
    pub(crate) history: VecDeque<HistoryLine>,
    pub(crate) cursor: Position,
    pub(crate) wrap_pending: bool,
    pub(crate) commands: CommandMarks,
    history_bytes: usize,
    // Indices in a monotonically numbered history queue. Only marked rows are
    // indexed, so publishing a frame never walks the whole scrollback.
    history_prompt_rows: VecDeque<usize>,
    history_timestamp_rows: VecDeque<usize>,
    history_origin: usize,
    scrollback: ScrollbackRegistry,
    prompt_epoch: u64,
    pack_scan: usize,
    pack_protection: Range<usize>,
    pub(crate) scrollback_compression_enabled: bool,
    keeps_history: bool,
    max_image_rows: usize,
}

impl Screen {
    pub(crate) fn scrollback_snapshot(&self) -> ScrollbackSnapshot {
        self.scrollback
            .snapshot(self.history_origin, self.history.len(), self.lines.len())
    }

    pub(crate) fn scrollback_offset(&self, anchor: &ScrollbackAnchor) -> usize {
        self.scrollback
            .offset(self.history_origin, self.history.len(), anchor)
    }

    pub(crate) fn live_history_start(&self) -> usize {
        self.history.partition_point(|line| line.archived)
    }

    pub(crate) fn new(columns: usize, rows: usize, keeps_history: bool) -> Self {
        Self {
            lines: (0..rows)
                .map(|_| Line::blank(columns, Style::default()))
                .collect(),
            history: VecDeque::new(),
            cursor: Position::default(),
            wrap_pending: false,
            commands: CommandMarks::default(),
            history_bytes: 0,
            history_prompt_rows: VecDeque::new(),
            history_timestamp_rows: VecDeque::new(),
            history_origin: 0,
            scrollback: ScrollbackRegistry::default(),
            prompt_epoch: 0,
            pack_scan: 0,
            pack_protection: 0..0,
            scrollback_compression_enabled: true,
            keeps_history,
            max_image_rows: 0,
        }
    }

    pub(crate) fn mark_prompt(&mut self, id: u64) {
        let line = &mut self.lines[self.cursor.row];
        let column = if line.cells[self.cursor.column].width == 0 {
            self.cursor.column.saturating_sub(1)
        } else {
            self.cursor.column
        };
        line.add_mark(LineMark {
            id,
            column,
            kind: LineMarkKind::Prompt,
        });
    }

    pub(crate) fn has_prompt_marks(&self) -> bool {
        !self.history_prompt_rows.is_empty() || self.lines.iter().any(Line::has_prompt)
    }

    pub(crate) fn prompt_layout(&self, current: Option<u64>) -> Option<PromptLayout> {
        let history = self.history.len();
        let mut rows = Vec::with_capacity(self.history_prompt_rows.len());
        let mut current_row = None;
        let mut collect = |row, marks: &[LineMark]| {
            if marks.iter().any(|mark| mark.kind == LineMarkKind::Prompt) {
                rows.push(row);
                if let Some(id) = current
                    && marks
                        .iter()
                        .any(|mark| mark.kind == LineMarkKind::Prompt && mark.id == id)
                {
                    current_row = Some(row);
                }
            }
        };
        for &index in &self.history_prompt_rows {
            let row = index.wrapping_sub(self.history_origin);
            collect(row, &self.history[row].marks);
        }
        for (row, line) in self.lines.iter().enumerate() {
            collect(history + row, &line.marks);
        }
        (!rows.is_empty()).then_some(PromptLayout {
            history,
            cursor_line: history + self.cursor.row,
            epoch: self.prompt_epoch,
            current: current_row,
            rows,
        })
    }

    pub(crate) fn put(
        &mut self,
        ch: char,
        width: usize,
        style: Style,
        hyperlink: Option<Arc<str>>,
        protected: bool,
        right: usize,
    ) {
        let line = &mut self.lines[self.cursor.row];
        let column = self.cursor.column;
        let blank = Cell::blank(style.erase());
        line.clear_glyph(column, &blank);
        if width == 2 {
            line.clear_glyph(column + 1, &blank);
        }
        line.cells[column] = Cell {
            text: ch.to_string(),
            width: width as u8,
            style,
            hyperlink: hyperlink.clone(),
            protected,
        };
        if width == 2 {
            line.cells[column + 1] = Cell::continuation(style, hyperlink, protected);
        }
        let next = column + width;
        self.wrap_pending = next > right;
        self.cursor.column = next.min(right);
    }

    pub(crate) fn append_combining(&mut self, ch: char) {
        let mut row = self.cursor.row;
        let mut column = self.cursor.column;
        if !self.wrap_pending {
            if column > 0 {
                column -= 1;
            } else if row > 0
                && let Some(length) = self.lines[row - 1].wrap_length
            {
                row -= 1;
                column = length.saturating_sub(1);
            } else {
                return;
            }
        }
        let line = &mut self.lines[row];
        if line.cells[column].width == 0 && column > 0 {
            column -= 1;
        }
        let text = &mut line.cells[column].text;
        if !text.is_empty() && text.len() < 128 {
            text.push(ch);
        }
    }

    pub(crate) fn mark_wrap(&mut self, used: usize, style: Style) {
        let row = self.cursor.row;
        let columns = self.lines[row].cells.len();
        self.erase_line(row, used, columns, style, false);
        self.lines[row].wrap_length = Some(used);
    }

    pub(crate) fn erase_line(
        &mut self,
        row: usize,
        start: usize,
        end: usize,
        style: Style,
        selective: bool,
    ) {
        if start >= end {
            return;
        }
        let line = &mut self.lines[row];
        let blank = Cell::blank(style);
        if selective {
            for column in start..end {
                if !line.cells[column].protected {
                    line.clear_glyph(column, &blank);
                }
            }
            if end == line.cells.len() && !line.cells[end - 1].protected {
                line.wrap_length = None;
            }
            return;
        }
        line.clear_glyph(start, &blank);
        line.clear_glyph(end - 1, &blank);
        line.cells[start..end].fill(blank);
        if end == line.cells.len() {
            line.wrap_length = None;
        }
    }

    pub(crate) fn insert_cells(&mut self, count: usize, style: Style, right: usize) {
        let row = &mut self.lines[self.cursor.row];
        let column = self.cursor.column;
        let count = count.min(right + 1 - column);
        let blank = Cell::blank(style);
        if row.cells[column].width == 0 {
            row.clear_glyph(column, &blank);
        }
        row.cells[column..=right].rotate_right(count);
        row.shift_marks(column, right, count as isize);
        row.cells[column..column + count].fill(blank.clone());
        row.repair_wide_cells(&blank);
    }

    pub(crate) fn delete_cells(&mut self, count: usize, style: Style, right: usize) {
        let row = &mut self.lines[self.cursor.row];
        let column = self.cursor.column;
        let count = count.min(right + 1 - column);
        let blank = Cell::blank(style);
        if row.cells[column].width == 0 {
            row.clear_glyph(column, &blank);
        }
        row.cells[column..=right].rotate_left(count);
        row.shift_marks(column, right, -(count as isize));
        let keep = right + 1 - count;
        row.cells[keep..=right].fill(blank.clone());
        row.repair_wide_cells(&blank);
    }

    pub(crate) fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        style: Style,
        record_history: bool,
    ) {
        let columns = self.lines[0].cells.len();
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);
        for _ in 0..count.min(bottom - top + 1) {
            let removed = self.lines.remove(top);
            self.lines.insert(bottom, Line::blank(columns, style));
            if record_history
                && top == 0
                && let Some(line) = removed
            {
                self.push_history(line);
            }
        }
    }

    pub(crate) fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, style: Style) {
        let columns = self.lines[0].cells.len();
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);
        for _ in 0..count.min(bottom - top + 1) {
            self.lines.remove(bottom);
            self.lines.insert(top, Line::blank(columns, style));
        }
    }

    pub(crate) fn scroll_rectangle(
        &mut self,
        rectangle: Rectangle,
        count: usize,
        upward: bool,
        style: Style,
    ) {
        let Rectangle {
            top,
            left,
            bottom,
            right,
        } = rectangle;
        let count = count.min(bottom - top + 1);
        if count == 0 {
            return;
        }
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);
        let lines = self.lines.make_contiguous();
        let blank = Cell::blank(style);

        if upward {
            for destination in top..bottom + 1 - count {
                let source = destination + count;
                let (before, after) = lines.split_at_mut(source);
                before[destination].cells[left..=right]
                    .clone_from_slice(&after[0].cells[left..=right]);
                before[destination].move_mark_columns(&mut after[0], left, right);
            }
            for line in &mut lines[bottom + 1 - count..=bottom] {
                line.cells[left..=right].fill(blank.clone());
                line.marks
                    .retain(|mark| !(left..=right).contains(&mark.column));
            }
        } else {
            for destination in (top + count..=bottom).rev() {
                let source = destination - count;
                let (before, after) = lines.split_at_mut(destination);
                after[0].cells[left..=right].clone_from_slice(&before[source].cells[left..=right]);
                after[0].move_mark_columns(&mut before[source], left, right);
            }
            for line in &mut lines[top..top + count] {
                line.cells[left..=right].fill(blank.clone());
                line.marks
                    .retain(|mark| !(left..=right).contains(&mark.column));
            }
        }
        for line in &mut lines[top..=bottom] {
            line.wrap_length = None;
            line.repair_wide_cells(&blank);
        }
    }

    pub(crate) fn shift_rectangle(&mut self, rectangle: Rectangle, forward: bool) {
        let blank = Cell::blank(Style::default());
        for row in rectangle.top..=rectangle.bottom {
            let line = &mut self.lines[row];
            let cells = &mut line.cells[rectangle.left..=rectangle.right];
            if forward {
                cells.rotate_left(1);
                cells[cells.len() - 1] = blank.clone();
            } else {
                cells.rotate_right(1);
                cells[0] = blank.clone();
            }
            line.shift_marks(
                rectangle.left,
                rectangle.right,
                if forward { -1 } else { 1 },
            );
            line.wrap_length = None;
            line.repair_wide_cells(&blank);
        }
    }

    pub(crate) fn fill_rectangle(&mut self, rectangle: Rectangle, cell: &Cell) {
        let blank = Cell::blank(Style::default());
        for row in rectangle.top..=rectangle.bottom {
            let line = &mut self.lines[row];
            line.clear_glyph(rectangle.left, &blank);
            line.clear_glyph(rectangle.right, &blank);
            line.cells[rectangle.left..=rectangle.right].fill(cell.clone());
        }
    }

    pub(crate) fn copy_rectangle(&mut self, rectangle: Rectangle, row: usize, column: usize) {
        let width = (rectangle.right - rectangle.left + 1).min(self.lines[0].cells.len() - column);
        let height = (rectangle.bottom - rectangle.top + 1).min(self.lines.len() - row);
        let copied = (rectangle.top..rectangle.top + height)
            .map(|index| self.lines[index].cells[rectangle.left..rectangle.left + width].to_vec())
            .collect::<Vec<_>>();
        let blank = Cell::blank(Style::default());
        for (offset, cells) in copied.into_iter().enumerate() {
            let line = &mut self.lines[row + offset];
            line.clear_glyph(column, &blank);
            line.clear_glyph(column + width - 1, &blank);
            line.cells[column..column + width].clone_from_slice(&cells);
            line.repair_wide_cells(&blank);
        }
    }

    pub(crate) fn take_history_from(&mut self, other: &mut Self) {
        self.history = std::mem::take(&mut other.history);
        self.history_bytes = std::mem::take(&mut other.history_bytes);
        self.history_prompt_rows = std::mem::take(&mut other.history_prompt_rows);
        self.history_timestamp_rows = std::mem::take(&mut other.history_timestamp_rows);
        self.history_origin = other.history_origin;
        self.scrollback = std::mem::take(&mut other.scrollback);
        self.prompt_epoch = other.prompt_epoch.wrapping_add(1);
        self.pack_scan = std::mem::take(&mut other.pack_scan);
        self.pack_protection = std::mem::take(&mut other.pack_protection);
        self.max_image_rows = other.max_image_rows;
    }

    pub(crate) fn clear_history(&mut self) {
        self.scrollback.invalidate();
        self.clear_history_contents();
    }

    fn clear_history_contents(&mut self) {
        self.history.clear();
        self.history_bytes = 0;
        self.history_prompt_rows.clear();
        self.history_timestamp_rows.clear();
        self.history_origin = 0;
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);
        self.pack_scan = 0;
        self.pack_protection = 0..0;
    }

    pub(crate) fn clear_live_prompts(&mut self) {
        for line in &mut self.lines {
            line.marks.clear();
        }
        self.commands = CommandMarks::default();
    }

    pub(crate) fn clear_buffer(&mut self) {
        let mut kept = self.lines.remove(self.cursor.row).expect("cursor line");
        let rows = self.lines.len() + 1;
        let columns = kept.cells.len();
        kept.marks.clear();
        self.clear_history();
        self.lines.clear();
        self.max_image_rows = kept
            .images
            .iter()
            .map(|image| usize::from(image.rows))
            .max()
            .unwrap_or(0);
        self.lines.push_back(kept);
        self.lines
            .resize_with(rows, || Line::blank(columns, Style::default()));
        self.cursor.row = 0;
        self.commands = CommandMarks::default();
    }

    pub(crate) fn compress_idle(&mut self, scroll_offset: usize) -> bool {
        if !self.scrollback_compression_enabled {
            return false;
        }
        let rows = self.lines.len();
        let end = self.history.len().saturating_sub(rows);
        let protection = if scroll_offset == 0 {
            0..0
        } else {
            let start = self.history.len().saturating_sub(scroll_offset);
            start.saturating_sub(rows)..(start + 2 * rows).min(end)
        };
        if self.pack_protection != protection {
            self.pack_protection = protection;
            self.pack_scan = 0;
        }
        self.pack_scan = self.pack_scan.min(end);
        let stop = (self.pack_scan + PACK_SCAN_ROWS).min(end);
        let deadline = Instant::now() + PACK_SLICE;
        while self.pack_scan < stop {
            if self.pack_protection.contains(&self.pack_scan) {
                self.pack_scan = self.pack_protection.end;
            } else {
                self.history_bytes -= self.history[self.pack_scan].pack();
                self.pack_scan += 1;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.pack_scan < end
    }

    pub(crate) fn clear_display(&mut self, style: Style) {
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);
        let blank = Cell::blank(style);
        for line in &mut self.lines {
            line.cells.fill(blank.clone());
            line.images.clear();
            line.wrap_length = None;
            line.hp_form = None;
            line.marks.clear();
        }
        self.commands = CommandMarks::default();
    }

    pub(crate) fn archive_display(&mut self, style: Style) {
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);
        let columns = self.lines[0].cells.len();
        let rows = self.lines.len();
        let mut lines = std::mem::take(&mut self.lines);
        while lines.back().is_some_and(Line::is_empty_for_history) {
            lines.pop_back();
        }
        if let Some(line) = lines.back_mut() {
            line.wrap_length = None;
        }
        for line in lines {
            self.push_history(line);
        }
        self.lines.resize_with(rows, || Line::blank(columns, style));
    }

    pub(crate) fn copy_reconnect_history(&self) -> Self {
        // A single blank live row provides a hard boundary when the history
        // is reflowed separately from the replacement's live grid.
        let mut copy = Self::new(self.lines[0].cells.len(), 1, true);
        for line in &self.history {
            let mut archived = line.clone();
            archived.archived = true;
            copy.push_history_line(archived);
        }
        if let Some(last) = self
            .lines
            .iter()
            .rposition(|line| !line.is_empty_for_history())
        {
            for line in self.lines.iter().take(last + 1) {
                let mut archived = HistoryLine::new(Line {
                    cells: line.cells.clone(),
                    images: line.images.clone(),
                    wrap_length: line.wrap_length,
                    hp_form: None,
                    marks: line.marks.clone(),
                    archived: true,
                });
                archived.pack();
                copy.push_history_line(archived);
            }
        }
        if let Some(last) = copy.history.back_mut() {
            last.wrap_length = None;
        }
        copy
    }

    /// Inserts older rows using the regular history budgets. Returns the
    /// number of inserted rows still retained after eviction.
    pub(crate) fn prepend_history(&mut self, mut older: Self) -> usize {
        if older.history.is_empty() {
            return 0;
        }
        let newer = std::mem::take(&mut self.history);
        let newer_count = newer.len();
        // Rebuild all row indexes and accounting while retaining packed rows.
        self.clear_history();
        for line in older.history.drain(..).chain(newer) {
            self.push_history_line(line);
        }
        let retained = self.history.len() - newer_count;
        if retained != 0 {
            self.commands.discard_truncated();
        }
        retained
    }

    pub(crate) fn update_history_image_height(&mut self, rows: usize) {
        self.max_image_rows = self.max_image_rows.max(rows);
    }

    pub(crate) fn place_image(&mut self, placement: KittyPlacement) {
        self.max_image_rows = self.max_image_rows.max(usize::from(placement.rows));
        let images = &mut self.lines[self.cursor.row].images;
        if images.len() >= MAX_PLACEMENTS_PER_LINE {
            images.remove(0);
        }
        images.push(placement);
    }

    pub(crate) fn previous_timestamp_marks(&self, row: usize) -> Option<&[LineMark]> {
        let index = self
            .history_timestamp_rows
            .partition_point(|&position| position.wrapping_sub(self.history_origin) < row)
            .checked_sub(1)?;
        let position = self.history_timestamp_rows[index].wrapping_sub(self.history_origin);
        self.history.get(position).map(|line| line.marks.as_slice())
    }

    pub(crate) fn marks_at(&self, row: usize) -> &[LineMark] {
        if row < self.history.len() {
            &self.history[row].marks
        } else {
            self.lines
                .get(row - self.history.len())
                .map_or(&[], |line| line.marks.as_slice())
        }
    }

    pub(crate) fn viewport_cells(&self, start: usize) -> (Vec<Cell>, Vec<Option<u16>>) {
        let rows = self.lines.len();
        let history_rows = (self.history.len() - start).min(rows);
        let mut cells = Vec::with_capacity(rows * self.lines[0].cells.len());
        let mut wrapped_columns = Vec::with_capacity(rows);
        for line in self.history.iter().skip(start).take(history_rows) {
            match line.cells() {
                Cow::Borrowed(row) => cells.extend_from_slice(row),
                Cow::Owned(row) => cells.extend(row),
            }
            wrapped_columns.push(line.wrap_length.map(|length| length as u16));
        }
        for line in self.lines.iter().take(rows - history_rows) {
            cells.extend_from_slice(&line.cells);
            wrapped_columns.push(line.wrap_length.map(|length| length as u16));
        }
        (cells, wrapped_columns)
    }

    pub(crate) fn viewport_images(&self, start: usize) -> Vec<ViewportImage> {
        if self.max_image_rows == 0 {
            return Vec::new();
        }
        let first = start.saturating_sub(self.max_image_rows - 1);
        let archive_end = self.live_history_start();
        // A tall image anchored before the reconnect boundary stays in the
        // scrollback even when its geometry would reach the live viewport.
        let first = if start >= archive_end {
            first.max(archive_end)
        } else {
            first
        };
        let mut images = self
            .history
            .iter()
            .map(|line| &line.images)
            .chain(self.lines.iter().map(|line| &line.images))
            .skip(first)
            .take(start + self.lines.len() - first)
            .enumerate()
            .flat_map(|(index, images)| {
                let row = (first + index) as i32 - start as i32;
                images
                    .iter()
                    .filter(move |placement| row + i32::from(placement.rows) > 0)
                    .map(move |placement| ViewportImage {
                        row,
                        placement: placement.clone(),
                    })
            })
            .collect::<Vec<_>>();
        images.sort_by_key(|image| image.placement.z_index);
        images
    }

    fn push_history(&mut self, line: Line) {
        if !self.keeps_history {
            return;
        }
        self.push_history_line(HistoryLine::new(line));
    }

    fn push_history_line(&mut self, line: HistoryLine) {
        self.history_bytes += line.bytes;
        if line.has_timestamp() {
            self.history_timestamp_rows
                .push_back(self.history_origin.wrapping_add(self.history.len()));
        }
        if line.has_prompt() {
            self.history_prompt_rows
                .push_back(self.history_origin.wrapping_add(self.history.len()));
        }
        self.history.push_back(line);
        while self.history.len() > MAX_HISTORY_LINES || self.history_bytes > MAX_HISTORY_BYTES {
            let Some(line) = self.history.pop_front() else {
                break;
            };
            self.history_bytes -= line.bytes;
            if line.has_prompt() {
                self.history_prompt_rows.pop_front();
            }
            if line.has_timestamp() {
                self.history_timestamp_rows.pop_front();
            }
            for mark in line.marks {
                self.commands.trim(mark);
            }
            self.history_origin = self.history_origin.wrapping_add(1);
            self.pack_scan = self.pack_scan.saturating_sub(1);
        }
    }

    pub(crate) fn resize(&mut self, columns: usize, rows: usize) {
        let old_columns = self.lines[0].cells.len();
        let old_rows = self.lines.len();
        if (old_columns, old_rows) == (columns, rows) {
            return;
        }
        self.prompt_epoch = self.prompt_epoch.wrapping_add(1);

        if columns != old_columns {
            let mut mapping = self
                .scrollback
                .has_snapshots()
                .then(|| ReflowMap::new(self.history_origin));
            let first_visible = self.history.len();
            let history = std::mem::take(&mut self.history);
            let visible = std::mem::take(&mut self.lines);
            let pack_history = self.scrollback_compression_enabled
                && history
                    .iter()
                    .any(|line| matches!(line.cells, HistoryCells::Packed(_)));
            self.clear_history_contents();
            let cursor_row = first_visible + self.cursor.row;
            let cursor = ReflowCursor {
                row: cursor_row,
                column: self.cursor.column,
                pending: self.wrap_pending,
            };
            let mut output = ReflowOutput::new(self, rows, pack_history);
            reflow(
                history
                    .into_iter()
                    .map(HistoryLine::into_line)
                    .chain(visible),
                columns,
                first_visible,
                cursor,
                &mut output,
                mapping.as_mut(),
            );
            let (cursor, visible_start) = output.finish();
            self.cursor = Position {
                row: cursor.row.saturating_sub(visible_start),
                column: cursor.column,
            };
            self.wrap_pending = cursor.pending;
            if let Some(mapping) = mapping {
                self.scrollback.reflow(&mapping);
            }
        }

        while self.cursor.row >= rows {
            if let Some(line) = self.lines.pop_front() {
                self.push_history(line);
            }
            self.cursor.row -= 1;
        }
        if columns == old_columns {
            while rows > self.lines.len() {
                let Some(line) = self.history.pop_back_if(|line| !line.archived) else {
                    break;
                };
                self.history_bytes -= line.bytes;
                if line.has_prompt() {
                    self.history_prompt_rows.pop_back();
                }
                if line.has_timestamp() {
                    self.history_timestamp_rows.pop_back();
                }
                self.lines.push_front(line.into_line());
                self.cursor.row += 1;
            }
        }
        self.lines.truncate(rows);
        self.lines
            .resize_with(rows, || Line::blank(columns, Style::default()));
        self.wrap_pending = false;
    }
}

#[derive(Clone, Copy, Default)]
struct ReflowCursor {
    row: usize,
    column: usize,
    pending: bool,
}

/// Keeps at most one screen of expanded output, even when a narrow resize
/// turns the compressed input into many more physical rows.
struct ReflowOutput<'a> {
    screen: &'a mut Screen,
    rows: usize,
    next_row: usize,
    visible_start: Option<usize>,
    cursor: Option<ReflowCursor>,
    pack_history: bool,
}

impl<'a> ReflowOutput<'a> {
    fn new(screen: &'a mut Screen, rows: usize, pack_history: bool) -> Self {
        Self {
            screen,
            rows,
            next_row: 0,
            visible_start: None,
            cursor: None,
            pack_history,
        }
    }

    fn push(&mut self, line: Line) {
        let row = self.next_row;
        self.next_row += 1;
        if let Some(start) = self.visible_start {
            debug_assert!(!line.archived);
            if self.screen.lines.len() == self.rows {
                if self.cursor.is_some_and(|cursor| cursor.row < row) {
                    return;
                }
                let previous = self.screen.lines.pop_front().expect("visible row");
                self.screen.push_history(previous);
                self.visible_start = Some(start + 1);
            }
            self.screen.lines.push_back(line);
        } else if self.screen.keeps_history {
            let mut line = HistoryLine::new(line);
            if self.pack_history {
                line.pack();
            }
            self.screen.push_history_line(line);
        }
    }

    fn finish(self) -> (ReflowCursor, usize) {
        let visible_start = self.visible_start.unwrap_or_default();
        let cursor = self.cursor.unwrap_or(ReflowCursor {
            row: visible_start,
            ..ReflowCursor::default()
        });
        (cursor, visible_start)
    }
}

fn reflow(
    source: impl Iterator<Item = Line>,
    columns: usize,
    first_visible: usize,
    original_cursor: ReflowCursor,
    output: &mut ReflowOutput<'_>,
    mut mapping: Option<&mut ReflowMap>,
) {
    let mut target = Line::blank(columns, Style::default());
    let mut next_column: usize = 0;

    for (row, mut line) in source.enumerate() {
        if row == first_visible {
            output.visible_start = Some(output.next_row);
        }
        target.archived = line.archived;
        let last_content = line
            .cells
            .iter()
            .rposition(|cell| !cell.is_default_blank())
            .map_or(0, |column| column + 1);
        let used = if let Some(length) = line.wrap_length {
            length
        } else if row == original_cursor.row {
            last_content.max(original_cursor.column + 1)
        } else {
            last_content
        };
        line.images.sort_by_key(|placement| placement.column);
        line.marks.sort_by_key(|mark| mark.column);
        let used = used
            .max(
                line.images
                    .last()
                    .map_or(0, |placement| usize::from(placement.column) + 1),
            )
            .max(line.marks.last().map_or(0, |mark| mark.column + 1));
        let mut images = line.images.into_iter().peekable();
        let mut marks = line.marks.into_iter().peekable();
        let old_columns = line.cells.len();
        if used == 0
            && let Some(mapping) = mapping.as_mut()
        {
            mapping.record(
                row,
                0,
                Position {
                    row: output.next_row,
                    column: next_column,
                },
            );
        }
        for (column, cell) in line.cells.into_iter().take(used).enumerate() {
            if cell.width == 0 {
                if let Some(mapping) = mapping.as_mut() {
                    mapping.record(
                        row,
                        column,
                        Position {
                            row: output.next_row,
                            column: next_column.saturating_sub(1),
                        },
                    );
                }
                reanchor_images(
                    &mut images,
                    &mut target,
                    column,
                    next_column.saturating_sub(1),
                );
                reanchor_marks(
                    &mut marks,
                    &mut target,
                    column,
                    next_column.saturating_sub(1),
                );
                if row == original_cursor.row
                    && column == original_cursor.column
                    && !original_cursor.pending
                {
                    output.cursor = Some(ReflowCursor {
                        row: output.next_row,
                        column: next_column.saturating_sub(1),
                        pending: false,
                    });
                }
                continue;
            }
            let width = usize::from(cell.width).min(columns);
            if next_column + width > columns {
                target.wrap_length = Some(next_column);
                output.push(target);
                target = Line::blank(columns, Style::default());
                target.archived = line.archived;
                next_column = 0;
            }
            reanchor_images(&mut images, &mut target, column, next_column);
            reanchor_marks(&mut marks, &mut target, column, next_column);
            if let Some(mapping) = mapping.as_mut() {
                mapping.record(
                    row,
                    column,
                    Position {
                        row: output.next_row,
                        column: next_column,
                    },
                );
            }
            if row == original_cursor.row
                && column == original_cursor.column
                && !original_cursor.pending
            {
                output.cursor = Some(ReflowCursor {
                    row: output.next_row,
                    column: next_column,
                    pending: false,
                });
            }
            if width == 2 {
                target.cells[next_column + 1] =
                    Cell::continuation(cell.style, cell.hyperlink.clone(), cell.protected);
            }
            target.cells[next_column] = Cell {
                width: width as u8,
                ..cell
            };
            next_column += width;
        }
        reanchor_marks(&mut marks, &mut target, old_columns, next_column);
        if row == original_cursor.row && original_cursor.pending {
            output.cursor = Some(ReflowCursor {
                row: output.next_row,
                column: next_column.min(columns - 1),
                pending: next_column == columns,
            });
        }
        if line.wrap_length.is_none() {
            output.push(target);
            target = Line::blank(columns, Style::default());
            next_column = 0;
        }
    }
    if next_column > 0 || output.next_row == 0 {
        output.push(target);
    }
}

fn reanchor_images(
    images: &mut std::iter::Peekable<impl Iterator<Item = KittyPlacement>>,
    target: &mut Line,
    old_column: usize,
    new_column: usize,
) {
    while let Some(mut placement) =
        images.next_if(|placement| usize::from(placement.column) == old_column)
    {
        placement.column = new_column as u16;
        target.images.push(placement);
    }
}

fn reanchor_marks(
    marks: &mut std::iter::Peekable<impl Iterator<Item = LineMark>>,
    target: &mut Line,
    old_column: usize,
    new_column: usize,
) {
    while let Some(mut mark) = marks.next_if(|mark| mark.column == old_column) {
        mark.column = new_column;
        target.add_mark(mark);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_rows_leave_space_without_changing_scrollback_limits() {
        let columns = 1024;
        let blank = || Line::blank(columns, Style::default());
        let raw_capacity = MAX_HISTORY_BYTES / blank().history_bytes();
        let mut screen = Screen::new(columns, 2, true);
        for _ in 0..raw_capacity + 20 {
            screen.push_history(blank());
        }
        assert_eq!(screen.history.len(), raw_capacity);
        assert!(screen.history_bytes <= MAX_HISTORY_BYTES);

        while screen.compress_idle(0) {}
        assert!(screen.history_bytes < MAX_HISTORY_BYTES / 4);
        for _ in 0..raw_capacity / 2 {
            screen.push_history(blank());
        }
        assert_eq!(screen.history.len(), raw_capacity + raw_capacity / 2);
        assert!(screen.history_bytes <= MAX_HISTORY_BYTES);
        assert_eq!(screen.history[0].cells().len(), columns);
        assert!(matches!(screen.history[0].cells(), Cow::Owned(_)));
    }
}
