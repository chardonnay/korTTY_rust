use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Instant;

use crate::{
    Cell, Terminal,
    command_timestamps::{Clock, CommandTimestampStatus},
    is_bidi_control,
    prompts::{LineMark, LineMarkKind},
    screen::{Position, Screen},
};

const MAX_FINISHED_COMMANDS: usize = 16_384;
const MAX_LAST_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECENT_OUTPUT_ROWS: usize = 200;
const MAX_RECENT_OUTPUT_BYTES: usize = 32 * 1024;

/// A half-open range in the current primary buffer. Rows count from the
/// oldest retained scrollback line. The range is invalidated by content edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastOutputRange {
    pub start_row: usize,
    pub start_column: u16,
    pub end_row: usize,
    pub end_column: u16,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastOutput {
    Disabled,
    AlternateScreen,
    NoMarks,
    NoCommand,
    NoOutput,
    Found(LastOutputRange),
}

#[derive(Default)]
pub(crate) struct CommandMarks {
    open: Option<OpenCommand>,
    finished: BTreeMap<u64, FinishedCommand>,
}

struct OpenCommand {
    id: u64,
    started: bool,
    truncated: bool,
    clock: Option<Clock>,
}

struct FinishedCommand {
    // A C mark lost by an edit is distinct and must never select unrelated text.
    truncated: bool,
    status: Option<CommandTimestampStatus>,
}

impl CommandMarks {
    pub(crate) fn discard_truncated(&mut self) {
        // These commands use the first retained row as a fallback start.
        // After prepending an earlier session, that row is unrelated.
        self.finished.retain(|_, command| !command.truncated);
        self.open.take_if(|open| open.truncated);
    }

    pub(crate) fn trim(&mut self, mark: LineMark) {
        match mark.kind {
            LineMarkKind::OutputStart => {
                if let Some(open) = self.open.as_mut().filter(|open| open.id == mark.id) {
                    open.truncated = true;
                }
                if let Some(command) = self.finished.get_mut(&mark.id) {
                    command.truncated = true;
                }
            }
            LineMarkKind::OutputEnd => {
                self.finished.remove(&mark.id);
            }
            LineMarkKind::Prompt | LineMarkKind::Timestamp(_) | LineMarkKind::Annotation => {}
        }
    }

    pub(crate) fn running_since(&self, entered: Instant) -> bool {
        self.open
            .as_ref()
            .and_then(|open| open.clock)
            .is_some_and(|clock| clock.now >= entered)
    }

    pub(crate) fn running_clock(&self, mark: LineMark) -> Option<Clock> {
        self.open.as_ref().filter(|open| open.id == mark.id)?.clock
    }

    pub(crate) fn finished_status(&self, mark: LineMark) -> Option<CommandTimestampStatus> {
        self.finished.get(&mark.id)?.status
    }
}

impl Screen {
    pub(crate) fn begin_command_block(&mut self, id: u64) {
        // The Java command store drops commands whose prompt is overwritten by
        // a new A on or above that prompt, including same-row prompt redraws.
        let overwritten = self
            .lines
            .iter()
            .skip(self.cursor.row)
            .flat_map(|line| line.marks.iter())
            .filter(|mark| mark.kind == LineMarkKind::Prompt)
            .map(|mark| mark.id)
            .min();
        if let Some(first) = overwritten {
            while self
                .commands
                .finished
                .last_key_value()
                .is_some_and(|(&id, _)| id >= first)
            {
                self.commands.finished.pop_last();
            }
        }
        self.commands.open = Some(OpenCommand {
            id,
            started: false,
            truncated: false,
            clock: None,
        });
        self.mark_prompt(id);
    }

    pub(crate) fn start_command_output(&mut self, clock: Option<Clock>) {
        let Some(open) = self.commands.open.as_mut().filter(|open| !open.started) else {
            return;
        };
        open.started = true;
        open.clock = clock;
        let id = open.id;
        self.mark_output(id, LineMarkKind::OutputStart);
    }

    pub(crate) fn finish_command_output(
        &mut self,
        clock: Option<Clock>,
        exit_code: Option<i32>,
    ) -> bool {
        let Some(open) = self.commands.open.take_if(|open| open.started) else {
            return false;
        };
        self.mark_output(open.id, LineMarkKind::OutputEnd);
        let status = clock
            .zip(open.clock)
            .map(|(end, start)| CommandTimestampStatus::Finished {
                exit_code,
                duration: end.now.saturating_duration_since(start.now),
            });
        self.commands.finished.insert(
            open.id,
            FinishedCommand {
                truncated: open.truncated,
                status,
            },
        );
        if self.commands.finished.len() > MAX_FINISHED_COMMANDS {
            self.commands.finished.pop_first();
        }
        true
    }

    fn mark_output(&mut self, id: u64, kind: LineMarkKind) {
        self.lines[self.cursor.row].add_mark(LineMark {
            id,
            kind,
            column: self.cursor.column + usize::from(self.wrap_pending),
        });
    }

    fn last_output_range(&self) -> LastOutput {
        let Some((&id, command)) = self.commands.finished.last_key_value() else {
            return if self.has_prompt_marks() || self.commands.open.is_some() {
                LastOutput::NoCommand
            } else {
                LastOutput::NoMarks
            };
        };
        let truncated = command.truncated;
        let Some((mut start, mut end)) = self.output_mark_positions(id, truncated) else {
            return LastOutput::NoCommand;
        };
        if start.row > end.row {
            return LastOutput::NoOutput;
        }
        if start.column > 0 && start.row < end.row && start.column >= self.line_text_end(start.row)
        {
            start.row += 1;
            start.column = 0;
        }
        end.column = end.column.min(self.line_text_end(end.row));
        while end.column == 0 && end.row > start.row {
            end.row -= 1;
            end.column = self.line_text_end(end.row);
        }
        if end.row == start.row && end.column <= start.column {
            return LastOutput::NoOutput;
        }
        LastOutput::Found(LastOutputRange {
            start_row: start.row,
            start_column: start.column as u16,
            end_row: end.row,
            end_column: end.column as u16,
            truncated,
        })
    }

    fn output_mark_positions(&self, id: u64, truncated: bool) -> Option<(Position, Position)> {
        let mut start = truncated.then_some(Position::default());
        let mut end = None;
        let history = self.history.len();
        let lines = self
            .lines
            .iter()
            .enumerate()
            .rev()
            .map(|(row, line)| (history + row, &line.marks))
            .chain(
                self.history
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(row, line)| (row, &line.marks)),
            );
        for (row, marks) in lines {
            for mark in marks.iter().filter(|mark| mark.id == id) {
                let position = Position {
                    row,
                    column: mark.column,
                };
                match mark.kind {
                    LineMarkKind::OutputStart if !truncated => start = Some(position),
                    LineMarkKind::OutputEnd => end = Some(position),
                    _ => {}
                }
            }
            if let (Some(start), Some(end)) = (start, end) {
                return Some((start, end));
            }
        }
        None
    }

    fn output_line(&self, row: usize) -> Option<(Cow<'_, [Cell]>, Option<usize>)> {
        let history = self.history.len();
        if row < history {
            let line = &self.history[row];
            return Some((line.cells(), line.wrap_length));
        }
        let line = self.lines.get(row - history)?;
        Some((Cow::Borrowed(&line.cells), line.wrap_length))
    }

    fn line_text_end(&self, row: usize) -> usize {
        self.output_line(row)
            .map_or(0, |(cells, _)| text_end(&cells))
    }

    fn output_text(&self, range: LastOutputRange) -> Option<String> {
        if range.start_row > range.end_row || range.end_row >= self.history.len() + self.lines.len()
        {
            return None;
        }
        let mut text = String::new();
        for row in range.start_row..=range.end_row {
            let (cells, wrap) = self.output_line(row)?;
            let Range { start, end } = output_columns(range, row, &cells, wrap);
            for (offset, cell) in cells.get(start..end).unwrap_or_default().iter().enumerate() {
                if cell.width == 0 || start + offset + usize::from(cell.width) > end {
                    continue;
                }
                let glyph = if cell.text.is_empty() {
                    " "
                } else {
                    &cell.text
                };
                if glyph.len() > MAX_LAST_OUTPUT_BYTES - text.len() {
                    return None;
                }
                text.push_str(glyph);
            }
            if row < range.end_row && wrap.is_none() {
                if text.len() == MAX_LAST_OUTPUT_BYTES {
                    return None;
                }
                text.push('\n');
            }
        }
        Some(text)
    }

    fn recent_output_text(&self, range: LastOutputRange) -> Option<(String, bool)> {
        let mut parts = Vec::new();
        let mut remaining = MAX_RECENT_OUTPUT_BYTES;
        let mut truncated = range.truncated;
        for row in (range.start_row..=range.end_row).rev() {
            let (cells, wrap) = self.output_line(row)?;
            let mut line = visible_output_line(&cells, output_columns(range, row, &cells, wrap));
            if parts.is_empty() || wrap.is_none() {
                line.truncate(line.trim_end().len());
            }
            if !parts.is_empty() && wrap.is_none() {
                line.push('\n');
            }
            if line.len() > remaining {
                truncated = true;
                if parts.is_empty() {
                    let start = line.ceil_char_boundary(line.len() - remaining);
                    parts.push(line.split_off(start));
                }
                break;
            }
            if !line.is_empty() {
                remaining -= line.len();
                parts.push(line);
            }
        }
        let mut text: String = parts.into_iter().rev().collect();
        let leading_blank_bytes: usize = text
            .split_inclusive('\n')
            .take_while(|line| line.trim().is_empty())
            .map(str::len)
            .sum();
        text.replace_range(..leading_blank_bytes, "");
        (!text.is_empty()).then_some((text, truncated))
    }

    fn recent_tail_range(&self) -> Option<LastOutputRange> {
        let history = self.history.len();
        let end_row = self
            .lines
            .iter()
            .rposition(|line| line.cells.iter().any(has_visible_output))
            .map(|row| history + row)
            .or_else(|| history.checked_sub(1))?;
        let start_row = (end_row + 1).saturating_sub(MAX_RECENT_OUTPUT_ROWS);
        Some(LastOutputRange {
            start_row,
            start_column: 0,
            end_row,
            end_column: self.lines[0].cells.len() as u16,
            truncated: start_row > 0,
        })
    }
}

fn output_columns(
    range: LastOutputRange,
    row: usize,
    cells: &[Cell],
    wrap: Option<usize>,
) -> Range<usize> {
    let start = if row == range.start_row {
        usize::from(range.start_column)
    } else {
        0
    };
    let limit = if row == range.end_row {
        usize::from(range.end_column)
    } else {
        cells.len()
    };
    start..limit.min(wrap.unwrap_or(cells.len())).min(text_end(cells))
}

fn has_visible_output(cell: &Cell) -> bool {
    !cell.style.hidden
        && cell
            .text
            .chars()
            .any(|ch| is_output_character(ch) && !ch.is_whitespace())
}

fn is_output_character(ch: char) -> bool {
    !ch.is_control() && !is_bidi_control(ch)
}

fn visible_output_line(cells: &[Cell], Range { start, end }: Range<usize>) -> String {
    let mut line = String::new();
    for (offset, cell) in cells.get(start..end).unwrap_or_default().iter().enumerate() {
        let width = usize::from(cell.width);
        if width == 0 || start + offset + width > end {
            continue;
        }
        if cell.style.hidden || cell.text.is_empty() {
            line.extend(std::iter::repeat_n(' ', width));
        } else {
            line.extend(cell.text.chars().filter(|&ch| is_output_character(ch)));
        }
    }
    line
}

fn text_end(cells: &[Cell]) -> usize {
    cells
        .iter()
        .rposition(|cell| !cell.text.is_empty())
        .map_or(0, |index| {
            (index + usize::from(cells[index].width)).min(cells.len())
        })
}

impl Terminal {
    /// Clears the normal buffer locally, keeping the cursor's physical line.
    /// Returns false while the alternate screen is active.
    pub fn clear_buffer(&mut self) -> bool {
        if self.model.alternate_screen {
            return false;
        }
        self.model.primary.clear_buffer();
        self.model.command_timing.clear_pending();
        // The Java widget follows the vendor clear with SithTerminal.scrollY().
        if self.model.origin_mode {
            self.model.primary.cursor.row = self.model.scroll_top;
        }
        self.model.current_prompt = None;
        true
    }

    /// Whether the active normal screen tracks a finished OSC 133 command.
    pub fn has_finished_command(&self) -> bool {
        self.model.prompt_navigation_enabled
            && !self.model.alternate_screen
            && !self.model.primary.commands.finished.is_empty()
    }

    /// Finds the newest finished command's output without decoding its text.
    pub fn last_output(&self) -> LastOutput {
        if !self.model.prompt_navigation_enabled {
            return LastOutput::Disabled;
        }
        if self.model.alternate_screen {
            return LastOutput::AlternateScreen;
        }
        self.model.primary.last_output_range()
    }

    /// Copies a range returned by `last_output` from the same content revision.
    /// Returns none for out-of-bounds ranges or text beyond the bounded copy size.
    pub fn last_output_text(&self, range: LastOutputRange) -> Option<String> {
        if self.model.alternate_screen || !self.model.prompt_navigation_enabled {
            return None;
        }
        self.model.primary.output_text(range)
    }

    /// Reads visible text from the newest finished command or the primary buffer tail.
    /// The boolean reports whether the capture starts after its source's beginning.
    pub fn recent_visible_output(&self) -> Result<(String, bool), LastOutput> {
        if self.model.alternate_screen {
            return Err(LastOutput::AlternateScreen);
        }
        let screen = &self.model.primary;
        if let LastOutput::Found(range) = self.last_output()
            && let Some(output) = screen.recent_output_text(range)
        {
            return Ok(output);
        }
        screen
            .recent_tail_range()
            .and_then(|range| screen.recent_output_text(range))
            .ok_or(LastOutput::NoOutput)
    }
}
