//! Live timestamps anchored to terminal marks rather than viewport row numbers.

use std::time::{Duration, Instant, SystemTime};

use crate::{
    Terminal,
    prompts::{LineMark, LineMarkKind},
    screen::Screen,
};

const QUIET_TIME: Duration = Duration::from_millis(500);

/// A command's status as reported by the terminal's shell integration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandTimestampStatus {
    /// OSC 133;C arrived and its matching completion is pending.
    Running { since: Instant },
    /// OSC 133;D completed a command after its OSC 133;C.
    Finished {
        exit_code: Option<i32>,
        duration: Duration,
    },
}

/// The timestamp and shell status of one visible terminal row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandTimestamp {
    /// Zero-based row relative to the viewport.
    pub row: u16,
    pub time: SystemTime,
    /// Nearest preceding retained timestamp, even just above the viewport.
    pub previous: Option<SystemTime>,
    pub status: Option<CommandTimestampStatus>,
}

#[derive(Clone, Copy)]
pub(crate) struct Clock {
    pub(crate) wall: SystemTime,
    pub(crate) now: Instant,
}

struct Pending {
    entered: Instant,
    anchor: u64,
    deadline: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct CommandTiming {
    pub(crate) clock: Option<Clock>,
    pending: Option<Pending>,
    serial: u64,
}

impl CommandTiming {
    pub(crate) fn after_reset(&self) -> Self {
        Self {
            clock: self.clock,
            serial: self.serial,
            pending: None,
        }
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending = None;
    }

    fn record(&mut self, screen: &mut Screen, row: usize, time: SystemTime) -> (u64, bool) {
        let line = &mut screen.lines[row];
        if let Some((id, _)) = line_timestamp(&line.marks) {
            return (id, false);
        }
        self.serial = self.serial.wrapping_add(1);
        let column = screen.cursor.column;
        // Keep the anchor within existing content so a timestamp cannot turn
        // trailing viewport padding into another physical row during reflow.
        let column = if line.cells[column].is_default_blank() {
            let last = line.cells.iter().rposition(|cell| !cell.is_default_blank());
            last.map_or(0, |last| last.min(column))
        } else {
            column
        };
        let column = if line.cells[column].width == 0 {
            column.saturating_sub(1)
        } else {
            column
        };
        line.add_mark(LineMark {
            id: self.serial,
            column,
            kind: LineMarkKind::Timestamp(time),
        });
        (self.serial, true)
    }

    pub(crate) fn completed(&mut self, screen: &mut Screen, clock: Clock) {
        let row = screen.cursor.row;
        self.record(screen, row, clock.wall);
        self.pending.take_if(|pending| clock.now >= pending.entered);
    }

    fn output(&mut self, screen: &Screen, now: Instant, shell_integration: bool) {
        let Some(pending) = self
            .pending
            .as_mut()
            .filter(|pending| now >= pending.entered)
        else {
            return;
        };
        pending.deadline = if shell_integration && screen.commands.running_since(pending.entered) {
            None
        } else {
            now.checked_add(QUIET_TIME)
        };
    }
}

impl Terminal {
    /// Records an Enter gesture at the live cursor, even while the gutter is hidden.
    pub fn record_command_enter(&mut self, wall: SystemTime, now: Instant) {
        let model = &mut self.model;
        if model.alternate_screen {
            return;
        }
        let row = model.primary.cursor.row;
        let (anchor, _) = model.command_timing.record(&mut model.primary, row, wall);
        model.command_timing.pending = Some(Pending {
            entered: now,
            anchor,
            deadline: None,
        });
    }

    /// Interprets bytes with host clocks for shell markers and completion debounce.
    /// Mark positions are captured inline as the parser consumes the stream.
    pub fn advance_timed(&mut self, input: &[u8], wall: SystemTime, now: Instant) {
        self.model.command_timing.clock = Some(Clock { wall, now });
        self.advance(input);
        let model = &mut self.model;
        model.command_timing.clock = None;
        if !input.is_empty() && !model.alternate_screen {
            model
                .command_timing
                .output(&model.primary, now, model.prompt_navigation_enabled);
        }
    }

    /// The quiet fallback deadline, if output followed a pending Enter without OSC 133;C.
    pub fn command_timestamp_deadline(&self) -> Option<Instant> {
        let model = &self.model;
        let pending = model.command_timing.pending.as_ref()?;
        if model.alternate_screen
            || (model.prompt_navigation_enabled
                && model.primary.commands.running_since(pending.entered))
        {
            return None;
        }
        pending.deadline
    }

    /// Records an eligible quiet completion. Returns whether a new mark was added.
    pub fn finish_quiet_command(&mut self, wall: SystemTime, now: Instant) -> bool {
        if !self
            .command_timestamp_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            return false;
        }
        let model = &mut self.model;
        let Some(pending) = model.command_timing.pending.take() else {
            return false;
        };
        let screen = &mut model.primary;
        let cursor = screen.cursor.row;
        // If this Enter's anchor is still visible and above it the cursor
        // moved backwards, leave the existing timestamp as the first mark.
        if screen.lines.iter().skip(cursor + 1).any(|line| {
            line.marks
                .iter()
                .any(|mark| mark.id == pending.anchor && mark.kind.is_timestamp())
        }) {
            return false;
        }
        model.command_timing.record(screen, cursor, wall).1
    }

    /// Returns metadata for the current viewport without decoding its cells.
    pub fn command_timestamps(&self, scroll_offset: usize) -> Vec<CommandTimestamp> {
        let model = &self.model;
        if model.alternate_screen {
            return Vec::new();
        }
        let screen = &model.primary;
        let start = screen.history.len().saturating_sub(scroll_offset);
        let mut previous = screen
            .previous_timestamp_marks(start)
            .and_then(line_timestamp)
            .map(|(_, time)| time);
        let mut result = Vec::new();
        for row in start..start + screen.lines.len() {
            let marks = screen.marks_at(row);
            let stamp = line_timestamp(marks).map(|(_, time)| time);
            let status = if model.prompt_navigation_enabled {
                row_status(screen, row, marks)
            } else {
                None
            };
            if let Some(time) = stamp.or_else(|| status.and_then(|status| status.wall)) {
                result.push(CommandTimestamp {
                    row: (row - start) as u16,
                    time,
                    previous,
                    status: status.map(|status| status.value),
                });
            }
            previous = stamp.or(previous);
        }
        result
    }
}

fn line_timestamp(marks: &[LineMark]) -> Option<(u64, SystemTime)> {
    marks
        .iter()
        .filter_map(|mark| match mark.kind {
            LineMarkKind::Timestamp(time) => Some((mark.id, time)),
            _ => None,
        })
        .min_by_key(|&(id, _)| id)
}

#[derive(Clone, Copy)]
struct StatusMark {
    id: u64,
    wall: Option<SystemTime>,
    value: CommandTimestampStatus,
}

fn row_status(screen: &Screen, row: usize, marks: &[LineMark]) -> Option<StatusMark> {
    let current = marks.iter().filter_map(|&mark| match mark.kind {
        LineMarkKind::OutputStart if mark.column == 0 => running_mark(screen, mark),
        LineMarkKind::OutputEnd => Some(StatusMark {
            id: mark.id,
            wall: None,
            value: screen.commands.finished_status(mark)?,
        }),
        _ => None,
    });
    let preceding = row
        .checked_sub(1)
        .into_iter()
        .flat_map(|previous| screen.marks_at(previous))
        .filter(|mark| mark.kind == LineMarkKind::OutputStart && mark.column > 0)
        .filter_map(|&mark| running_mark(screen, mark));
    current.chain(preceding).max_by_key(|mark| mark.id)
}

fn running_mark(screen: &Screen, mark: LineMark) -> Option<StatusMark> {
    let clock = screen.commands.running_clock(mark)?;
    Some(StatusMark {
        id: mark.id,
        wall: Some(clock.wall),
        value: CommandTimestampStatus::Running { since: clock.now },
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::time::UNIX_EPOCH;

    use super::*;

    fn wall(millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(millis)
    }

    #[test]
    fn command_timestamps_record_first_enter_and_debounce_quiet_output() {
        let mut terminal = Terminal::new(20, 4);
        let start = Instant::now();
        terminal.advance(b"$ id");
        terminal.record_command_enter(wall(100), start);
        terminal.record_command_enter(wall(200), start + Duration::from_millis(100));
        assert_eq!(terminal.command_timestamp_deadline(), None);
        assert_eq!(terminal.command_timestamps(0)[0].time, wall(100));

        let first_output = start + Duration::from_millis(150);
        terminal.advance_timed(b"\r\nresponse", wall(250), first_output);
        assert_eq!(
            terminal.command_timestamp_deadline(),
            Some(first_output + QUIET_TIME)
        );
        assert!(!terminal.finish_quiet_command(wall(750), first_output));
        let second_output = start + Duration::from_millis(400);
        terminal.advance_timed(b"\r\n$ ", wall(500), second_output);
        assert_eq!(
            terminal.command_timestamp_deadline(),
            Some(second_output + QUIET_TIME)
        );
        assert!(terminal.finish_quiet_command(wall(1000), second_output + QUIET_TIME));
        assert_eq!(
            terminal.command_timestamps(0),
            [
                CommandTimestamp {
                    row: 0,
                    time: wall(100),
                    previous: None,
                    status: None
                },
                CommandTimestamp {
                    row: 2,
                    time: wall(1000),
                    previous: Some(wall(100)),
                    status: None
                },
            ]
        );
        assert_eq!(terminal.command_timestamp_deadline(), None);
    }

    #[test]
    fn command_timestamps_follow_exact_c_and_d_parser_positions() {
        let mut terminal = Terminal::new(20, 6);
        let start = Instant::now();
        terminal.advance_timed(b"\x1b]133;A\x07$ sleep", wall(100), start);
        terminal.record_command_enter(wall(200), start + Duration::from_millis(100));
        let output = start + Duration::from_millis(200);
        terminal.advance_timed(b"\x1b]133;C\x07\r\nfirst", wall(300), output);
        let marks = terminal.command_timestamps(0);
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[1].row, 1);
        assert_eq!(
            marks[1].status,
            Some(CommandTimestampStatus::Running { since: output })
        );
        assert_eq!(terminal.command_timestamp_deadline(), None);
        assert!(!terminal.finish_quiet_command(wall(900), start + Duration::from_millis(800)));

        terminal.advance_timed(
            b"\r\nsecond\r\n\x1b]133;D;23\x07\x1b]133;A\x07$ \r\nnext",
            wall(1000),
            start + Duration::from_millis(900),
        );
        let marks = terminal.command_timestamps(0);
        assert_eq!(marks.len(), 2);
        assert_eq!(
            marks[1],
            CommandTimestamp {
                row: 3,
                time: wall(1000),
                previous: Some(wall(200)),
                status: Some(CommandTimestampStatus::Finished {
                    exit_code: Some(23),
                    duration: Duration::from_millis(700),
                }),
            }
        );
        assert_eq!(terminal.command_timestamp_deadline(), None);
        terminal.set_prompt_navigation_enabled(false);
        assert_eq!(terminal.command_timestamps(0)[1].status, None);
    }

    #[test]
    fn command_timestamps_ignore_orphan_d_and_suspend_on_the_alternate_screen() {
        let mut terminal = Terminal::new(20, 4);
        let now = Instant::now();
        terminal.advance_timed(b"\x1b]133;D;1\x07", wall(100), now);
        assert!(terminal.command_timestamps(0).is_empty());

        terminal.record_command_enter(wall(200), now);
        terminal.advance_timed(b"\x1b[?1049h\x1b]133;A\x07\x1b]133;C\x07", wall(300), now);
        terminal.record_command_enter(wall(300), now);
        terminal.advance_timed(b"\x1b]133;D;0\x07", wall(400), now);
        assert!(terminal.command_timestamps(0).is_empty());
        assert_eq!(terminal.command_timestamp_deadline(), None);

        terminal.advance_timed(b"\x1b[?1049l", wall(500), now);
        assert_eq!(terminal.command_timestamps(0)[0].time, wall(200));
        terminal.advance_timed(b"\x1b[3J", wall(600), now);
        assert!(terminal.command_timestamps(0).is_empty());
        assert_eq!(terminal.command_timestamp_deadline(), None);
    }

    #[test]
    fn command_timestamps_follow_packed_history_through_reflow_and_reconnect() {
        let mut terminal = Terminal::new(8, 3);
        let now = Instant::now();
        terminal.advance(b"abcdefghij");
        terminal.record_command_enter(wall(100), now);
        terminal.advance(b"\r\nsecond");
        terminal.record_command_enter(wall(200), now);
        terminal.advance(b"\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\nseventh");
        while terminal.compress_idle_scrollback(0) {}
        assert!(matches!(terminal.history_line(1), Some(Cow::Owned(_))));
        assert_eq!(
            terminal
                .command_timestamps(terminal.snapshot().scrollback_lines)
                .iter()
                .map(|mark| (mark.row, mark.time))
                .collect::<Vec<_>>(),
            [(1, wall(100)), (2, wall(200))]
        );
        let scrolled = terminal.command_timestamps(terminal.snapshot().scrollback_lines - 2);
        assert_eq!(scrolled[0].previous, Some(wall(100)));

        terminal.resize(16, 3);
        let from_top = terminal.snapshot().scrollback_lines;
        assert_eq!(
            terminal
                .command_timestamps(from_top)
                .iter()
                .map(|mark| (mark.row, mark.time))
                .collect::<Vec<_>>(),
            [(0, wall(100)), (1, wall(200))]
        );

        let mut next = Terminal::new(8, 3);
        next.prepend_reconnect_history(terminal.reconnect_history());
        assert_eq!(
            next.command_timestamps(next.snapshot().scrollback_lines)
                .iter()
                .map(|mark| (mark.row, mark.time))
                .collect::<Vec<_>>(),
            [(1, wall(100)), (2, wall(200))]
        );
        next.advance(b"\x1b[3J");
        assert!(next.command_timestamps(0).is_empty());
    }

    #[test]
    fn command_timestamp_anchor_does_not_turn_trailing_padding_into_an_extra_row() {
        let mut terminal = Terminal::new(16, 2);
        terminal.advance(b"12345678");
        terminal.record_command_enter(wall(100), Instant::now());
        terminal.advance(b"\r\nsecond\r\nthird");
        terminal.resize(8, 2);
        let history = terminal.snapshot().scrollback_lines;
        assert_eq!(history, 1);
        assert_eq!(terminal.viewport_snapshot(history).wrapped_columns(0), None);
        assert_eq!(terminal.command_timestamps(history)[0].row, 0);
    }

    #[test]
    fn command_timestamps_evicted_from_history_are_not_used_as_previous_marks() {
        let mut terminal = Terminal::new(8, 2);
        let now = Instant::now();
        terminal.advance(b"first");
        terminal.record_command_enter(wall(100), now);
        for _ in 0..10_005 {
            terminal.advance(b"\r\noutput");
        }
        assert_eq!(terminal.snapshot().scrollback_lines, 10_000);
        terminal.record_command_enter(wall(200), now);
        assert_eq!(
            terminal.command_timestamps(0),
            [CommandTimestamp {
                row: 1,
                time: wall(200),
                previous: None,
                status: None
            }]
        );
    }
}
