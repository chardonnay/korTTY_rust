//! An agent history projection over the terminal's physical rows.
//!
//! Its rows never enter the terminal parser, control API, or terminal recordings.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::keyboard::{self, Modifiers, key::Named};
use kortty_terminal::{ReconnectHistory, ScrollbackAnchor, ScrollbackSnapshot, Terminal};

use crate::terminal_agent::design::inline::{self, Line, State, StepState, Style, Transcript};

use super::{Frame as TerminalFrame, SearchState, Status, ViewRequest, make_frame};

const MAX_BLOCKS: usize = 16;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct Update {
    pub id: u64,
    pub epoch: u64,
    pub style: Style,
    pub transcript: Transcript,
    pub animations: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Details,
    Dismiss,
    Approve,
    Reject,
}

impl Key {
    pub(crate) fn from_event(
        key: &keyboard::Key,
        modifiers: Modifiers,
        approval: bool,
    ) -> Option<Self> {
        match (key, modifiers) {
            (keyboard::Key::Character(value), Modifiers::CTRL)
                if value.eq_ignore_ascii_case("r") =>
            {
                Some(Self::Details)
            }
            (keyboard::Key::Named(Named::Escape), modifiers) if modifiers.is_empty() => {
                Some(Self::Dismiss)
            }
            (keyboard::Key::Character(value), modifiers)
                if approval && modifiers.is_empty() && value == "1" =>
            {
                Some(Self::Approve)
            }
            (keyboard::Key::Character(value), modifiers)
                if approval && modifiers.is_empty() && value == "2" =>
            {
                Some(Self::Reject)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Interaction {
    pub binding: crate::terminal_view::agent_cursor::Binding,
    pub approval: bool,
}

pub(super) fn queue(pending: &mut VecDeque<Update>, update: Update) {
    if let Some(current) = pending.iter_mut().find(|item| item.id == update.id) {
        *current = update;
    } else {
        if pending.len() == MAX_BLOCKS {
            pending.pop_front();
        }
        pending.push_back(update);
    }
}

struct Record {
    update: Update,
    marked: bool,
    // Archived records are static even when the source used Focus mode.
    since: Option<Instant>,
    rendered: Option<Rendered>,
}

struct Rendered {
    columns: u16,
    lines: Arc<[Line]>,
    approval: bool,
}

impl Record {
    fn active(&self) -> bool {
        matches!(self.update.transcript.state, State::Running | State::Paused)
    }
}

#[derive(Default)]
pub(super) struct History(VecDeque<Record>);

pub(super) struct Reconnect {
    terminal: ReconnectHistory,
    records: Vec<Update>,
}

impl History {
    pub(super) fn apply(&mut self, terminal: &mut Terminal, pending: VecDeque<Update>) -> bool {
        let changed = !pending.is_empty();
        let now = Instant::now();
        for update in pending {
            if let Some(record) = self
                .0
                .iter_mut()
                .find(|record| record.update.id == update.id)
            {
                let Some(since) = &mut record.since else {
                    continue;
                };
                if record.update.transcript.state != update.transcript.state {
                    *since = now;
                }
                record.update = update;
                record.rendered = None;
            } else {
                if self.0.len() == MAX_BLOCKS
                    && let Some(removed) = self.0.pop_front()
                {
                    terminal.remove_presentation_row(removed.update.id);
                }
                self.0.push_back(Record {
                    update,
                    marked: false,
                    since: Some(now),
                    rendered: None,
                });
            }
        }
        changed
    }

    pub(super) fn capture_reconnect(&self, terminal: &Terminal) -> Reconnect {
        let marks = terminal.presentation_rows();
        let records = self
            .0
            .iter()
            .filter_map(|record| {
                if !record.marked
                    || !record.update.transcript.state.finished()
                    || marks
                        .iter()
                        .filter(|(id, _)| *id == record.update.id)
                        .count()
                        != 1
                {
                    return None;
                }
                let mut update = record.update.clone();
                update.animations = false;
                update.transcript.prompt = None;
                update
                    .transcript
                    .steps
                    .retain(|step| step.state != StepState::Running);
                Some(update)
            })
            .collect::<Vec<_>>();
        let ids = records.iter().map(|update| update.id).collect::<Vec<_>>();
        Reconnect {
            terminal: terminal.reconnect_history_with_presentation(&ids),
            records,
        }
    }

    pub(super) fn prepend_reconnect(&mut self, terminal: &mut Terminal, mut older: Reconnect) {
        older
            .records
            .retain(|update| !self.0.iter().any(|record| record.update.id == update.id));
        let ids = older
            .records
            .iter()
            .map(|update| update.id)
            .collect::<Vec<_>>();
        let retained = terminal.prepend_reconnect_history_with_presentation(older.terminal, &ids);
        for update in older.records.into_iter().rev() {
            if !retained.contains(&update.id) {
                continue;
            }
            if self.0.len() == MAX_BLOCKS {
                terminal.remove_presentation_row(update.id);
                continue;
            }
            self.0.push_front(Record {
                update,
                marked: true,
                since: None,
                rendered: None,
            });
        }
    }

    pub(super) fn frame(
        &mut self,
        terminal: &mut Terminal,
        status: Status,
        view: &ViewRequest,
        search: &SearchState,
    ) -> TerminalFrame {
        let raw_anchor = view
            .inline_anchor
            .as_ref()
            .map(|anchor| &anchor.raw)
            .or(view.scroll_anchor.as_ref());
        let raw_offset = terminal.scrollback_offset(raw_anchor);
        if self.0.is_empty() || terminal.alternate_screen_active() {
            return make_frame(terminal, status, raw_offset, search);
        }
        let mut marks = terminal.presentation_rows();
        self.0.retain_mut(|record| {
            if marks.iter().any(|(id, _)| *id == record.update.id) {
                return true;
            }
            if record.marked && !record.active() {
                return false;
            }
            terminal.mark_presentation_row(record.update.id);
            record.marked = true;
            true
        });
        marks = terminal.presentation_rows();
        let mut frame = make_frame(terminal, status, raw_offset, search);
        if frame.screen.mouse_reporting() {
            return frame;
        }
        let rows = frame.screen.rows;
        let columns = frame.screen.columns;
        let history = frame.screen.scrollback_lines;
        let cursor = history + usize::from(frame.screen.cursor.row);
        let layout = Layout::new(
            history,
            rows,
            columns,
            cursor,
            terminal.presentation_blank_tail(),
            marks,
            &mut self.0,
        );
        let Some(layout) = layout else {
            return frame;
        };
        let coordinates = frame
            .scrollback
            .as_ref()
            .expect("stream scrollback")
            .clone();
        let raw_top = history.saturating_sub(raw_offset);
        let top = view
            .inline_anchor
            .as_ref()
            .map_or_else(
                || {
                    if view.scroll_anchor.is_some() && raw_offset != 0 {
                        layout.raw_to_display(raw_top)
                    } else {
                        layout.history
                    }
                },
                |anchor| layout.top(&coordinates, anchor),
            )
            .min(layout.history);
        let raw_start = layout.first_raw(top).min(history);
        if raw_start != raw_top {
            frame = make_frame(terminal, frame.status.clone(), history - raw_start, search);
        }
        frame.inline = Some(Arc::new(Frame {
            coordinates,
            layout,
            top,
            raw_start,
            rows,
            cursor,
        }));
        frame
    }
}

struct Block {
    id: u64,
    epoch: u64,
    raw: usize,
    start: usize,
    lines: Arc<[Line]>,
    approval: bool,
    focus: Option<(Instant, bool)>,
}

struct Layout {
    blocks: Vec<Block>,
    history: usize,
    total: usize,
}

enum Row<'a> {
    Raw(usize),
    Block(&'a Block, usize),
}

impl Layout {
    fn new(
        history: usize,
        rows: u16,
        columns: u16,
        cursor: usize,
        blank_tail: usize,
        marks: Vec<(u64, usize)>,
        records: &mut VecDeque<Record>,
    ) -> Option<Self> {
        let mut extra = 0;
        let mut blocks = Vec::with_capacity(marks.len());
        for (id, raw) in marks {
            let Some(record) = records.iter_mut().find(|record| record.update.id == id) else {
                continue;
            };
            if record
                .rendered
                .as_ref()
                .is_none_or(|rendered| rendered.columns != columns)
            {
                record.rendered = Some(render(&record.update, columns));
            }
            let rendered = record.rendered.as_ref().expect("layout for this width");
            let lines = &rendered.lines;
            if lines.is_empty() {
                continue;
            }
            blocks.push(Block {
                id,
                epoch: record.update.epoch,
                raw,
                start: raw + extra,
                lines: Arc::clone(lines),
                approval: rendered.approval,
                focus: record
                    .since
                    .filter(|_| record.update.style == Style::Focus)
                    .map(|since| (since, !record.active())),
            });
            extra += lines.len();
        }
        let mut start = history + extra;
        if let Some(block) = blocks
            .iter()
            .rev()
            .find(|block| (history..=cursor).contains(&block.raw))
        {
            start -= start.saturating_sub(block.start).min(blank_tail);
        }
        (!blocks.is_empty()).then_some(Self {
            blocks,
            history: start,
            total: history + usize::from(rows) + extra,
        })
    }

    fn raw_to_display(&self, raw: usize) -> usize {
        raw + self
            .blocks
            .iter()
            .take_while(|block| block.raw <= raw)
            .map(|block| block.lines.len())
            .sum::<usize>()
    }

    fn row(&self, index: usize) -> Option<Row<'_>> {
        if index >= self.total {
            return None;
        }
        let mut extra = 0;
        for block in &self.blocks {
            if index < block.start {
                break;
            }
            let index = index - block.start;
            if index < block.lines.len() {
                return Some(Row::Block(block, index));
            }
            extra += block.lines.len();
        }
        Some(Row::Raw(index - extra))
    }

    fn first_raw(&self, index: usize) -> usize {
        match self.row(index) {
            Some(Row::Raw(raw)) => raw,
            Some(Row::Block(block, _)) => block.raw,
            None => self.total,
        }
    }

    fn top(&self, coordinates: &ScrollbackSnapshot, anchor: &Anchor) -> usize {
        if let Some((id, row)) = anchor.block
            && let Some(block) = self.blocks.iter().find(|block| block.id == id)
        {
            return block.start + row.min(block.lines.len() - 1);
        }
        coordinates
            .row(&anchor.raw)
            .map_or(self.history, |raw| self.raw_to_display(raw))
    }
}

fn render(update: &Update, columns: u16) -> Rendered {
    let columns_usize = usize::from(columns);
    let mut transcript = update.transcript.clone();
    let spinner = if update.animations {
        ["◴", "◷", "◶", "◵"][(transcript.elapsed_ms / 1000 % 4) as usize]
    } else {
        "…"
    };
    let mut lines = inline::render(update.style, &transcript, columns_usize, spinner);
    let mut approval = false;
    if let Some(inline::Prompt::Approval {
        message,
        commands,
        keyboard: true,
        ..
    }) = &transcript.prompt
    {
        // Check the rendered review footer independently of earlier actions.
        let exact = |line: &Line, mark: inline::Role, role: inline::Role, text: &str| {
            line.segments
                .get(1)
                .is_some_and(|segment| segment.role == mark)
                && line
                    .segments
                    .last()
                    .is_some_and(|segment| segment.role == role && segment.text == text)
        };
        if let [command] = commands.as_slice()
            && let [.., destination, command_line, footer] = lines.as_slice()
        {
            approval = !command.trim().is_empty()
                && !command.contains(['\r', '\n', '\t'])
                && exact(destination, inline::Role::Warn, inline::Role::Text, message)
                && exact(
                    command_line,
                    inline::Role::Accent,
                    inline::Role::Title,
                    command,
                )
                && footer
                    .segments
                    .iter()
                    .any(|segment| segment.role == inline::Role::Key);
        }
        if !approval && let Some(inline::Prompt::Approval { keyboard, .. }) = &mut transcript.prompt
        {
            *keyboard = false;
            lines = inline::render(update.style, &transcript, columns_usize, spinner);
        }
    }
    Rendered {
        columns,
        lines: lines.into(),
        approval,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub(super) raw: ScrollbackAnchor,
    block: Option<(u64, usize)>,
}

pub(crate) struct Frame {
    coordinates: ScrollbackSnapshot,
    layout: Layout,
    top: usize,
    raw_start: usize,
    rows: u16,
    cursor: usize,
}

impl Frame {
    pub(crate) fn history(&self) -> usize {
        self.layout.history
    }

    pub(crate) fn display_of_raw(&self, row: usize) -> usize {
        self.layout.raw_to_display(row)
    }

    pub(crate) fn offset(&self) -> usize {
        self.layout.history - self.top
    }

    pub(crate) fn offset_of(&self, anchor: Option<&Anchor>) -> usize {
        anchor.map_or(0, |anchor| {
            self.layout
                .history
                .saturating_sub(self.layout.top(&self.coordinates, anchor))
        })
    }

    pub(crate) fn anchor(&self, offset: usize) -> Option<Anchor> {
        if offset == 0 {
            return None;
        }
        let top = self.layout.history.saturating_sub(offset);
        let (raw, block) = match self.layout.row(top)? {
            Row::Raw(raw) => (raw, None),
            Row::Block(block, row) => (block.raw, Some((block.id, row))),
        };
        Some(Anchor {
            raw: self.coordinates.row_anchor(raw)?,
            block,
        })
    }

    pub(crate) fn display_raw_row(&self, display: u16) -> Option<u16> {
        if display >= self.rows {
            return None;
        }
        match self.layout.row(self.top + usize::from(display))? {
            Row::Raw(raw) => u16::try_from(raw.checked_sub(self.raw_start)?)
                .ok()
                .filter(|raw| *raw < self.rows),
            Row::Block(..) => None,
        }
    }

    pub(crate) fn raw_display_row(&self, raw: u16) -> Option<u16> {
        let index = self
            .layout
            .raw_to_display(self.raw_start + usize::from(raw));
        u16::try_from(index.checked_sub(self.top)?)
            .ok()
            .filter(|display| *display < self.rows)
    }

    pub(crate) fn line(&self, display: u16) -> Option<&Line> {
        match self.layout.row(self.top + usize::from(display))? {
            Row::Block(block, row) => block.lines.get(row),
            Row::Raw(_) => None,
        }
    }

    pub(crate) fn line_identity(&self, display: u16) -> Option<(u64, u64, usize)> {
        match self.layout.row(self.top + usize::from(display))? {
            Row::Block(block, row) => Some((block.id, block.epoch, row)),
            Row::Raw(_) => None,
        }
    }

    pub(crate) fn visible(&self, id: u64, epoch: u64) -> bool {
        self.layout.blocks.iter().any(|block| {
            block.id == id
                && block.epoch == epoch
                && block.start < self.top + usize::from(self.rows)
                && block.start + block.lines.len() > self.top
        })
    }

    pub(crate) fn approval_visible(&self, id: u64, epoch: u64) -> bool {
        self.layout.blocks.iter().any(|block| {
            block.id == id
                && block.epoch == epoch
                && block.approval
                && block.start >= self.top
                && block.start + block.lines.len() <= self.top + usize::from(self.rows)
        })
    }

    fn focus_block(&self) -> Option<&Block> {
        (self.offset() == 0)
            .then(|| {
                self.layout.blocks.iter().rev().find(|block| {
                    block.focus.is_some_and(|(since, finished)| {
                        !finished || since.elapsed() < Duration::from_millis(1220)
                    }) && block.start < self.top + usize::from(self.rows)
                        && block.start + block.lines.len() > self.top
                })
            })
            .flatten()
    }

    pub(crate) fn focus(&self) -> Option<Range<u16>> {
        let block = self.focus_block()?;
        Some(
            block.start.saturating_sub(self.top) as u16
                ..(block.start + block.lines.len() - self.top).min(usize::from(self.rows)) as u16,
        )
    }

    pub(crate) fn focus_bright_end(&self) -> u16 {
        self.layout
            .raw_to_display(self.cursor)
            .saturating_sub(self.top)
            .saturating_add(1)
            .min(usize::from(self.rows)) as u16
    }

    pub(crate) fn focus_age(&self, now: Instant) -> Option<Duration> {
        self.focus_block()?
            .focus
            .map(|(since, _)| now.saturating_duration_since(since))
    }

    pub(crate) fn focus_finished(&self) -> bool {
        self.focus_block()
            .is_some_and(|block| block.focus.is_some_and(|(_, finished)| finished))
    }
}

#[cfg(test)]
pub(crate) fn fixture(frame: &mut TerminalFrame, raw: u16, lines: Vec<Line>, focus: bool) {
    let rows = frame.screen.rows;
    let history = frame.screen.scrollback_lines;
    let raw_start = history.saturating_sub(frame.scroll_offset);
    let raw = raw_start + usize::from(raw);
    let extra = lines.len();
    let layout = Layout {
        blocks: vec![Block {
            id: 1,
            epoch: 0,
            raw,
            start: raw,
            lines: lines.into(),
            approval: false,
            focus: focus.then(|| (Instant::now(), false)),
        }],
        history: raw_start,
        total: history + usize::from(rows) + extra,
    };
    let terminal = Terminal::new(frame.screen.columns, rows);
    let coordinates = frame
        .scrollback
        .clone()
        .unwrap_or_else(|| terminal.scrollback_snapshot());
    frame.inline = Some(Arc::new(Frame {
        coordinates,
        layout,
        top: raw_start,
        raw_start,
        rows,
        cursor: history + usize::from(frame.screen.cursor.row),
    }));
}
