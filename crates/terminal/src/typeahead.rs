//! Conservative, display-only predictions for an echoing shell input line.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use unicode_width::UnicodeWidthChar;

use crate::{Cell, Color, Style, Terminal, TerminalProfile, is_bidi_control};

const LATENCY_THRESHOLD: Duration = Duration::from_millis(100);
const MAX_TERMINAL_DELAY: Duration = Duration::from_millis(1500);
const LATENCY_SAMPLES: usize = 30;
const MAX_PENDING: usize = 32;
const MAX_INPUT_BYTES: usize = 256;
const MAX_GLYPH_BYTES: usize = 128;
const ZSH_WORD_CHARS: &str = "*?_-.[]~=/&;!#$%^(){}<>";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TypeAheadShell {
    Bash,
    Zsh,
    #[default]
    Unknown,
}

/// A replacement row and cursor for rendering only. The authoritative cells
/// remain available to search, selection, protocol replies and recording.
#[derive(Clone, Copy, Debug)]
pub struct TypeAheadOverlay<'a> {
    pub row: u16,
    pub cursor_column: u16,
    pub cells: &'a [Cell],
}

/// A per-session predictor. Times come from the owning session, and no
/// predicted cell is written into the terminal or sent to the transport.
pub struct TypeAhead {
    shell: TypeAheadShell,
    latency_threshold: Duration,
    latencies: VecDeque<Duration>,
    showing: bool,
    out_of_sync: bool,
    last_input: Option<Instant>,
    confirmed: Option<Confirmed>,
    pending: VecDeque<Prediction>,
    next_id: u64,
}

struct Confirmed {
    anchor: Anchor,
    line: Row,
    floor: usize,
    has_character_echo: bool,
}

struct Prediction {
    id: u64,
    created_at: Instant,
    edit: Edit,
    line: Row,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Anchor {
    row: usize,
    rows: usize,
    columns: usize,
    history: usize,
}

struct Observed<'a> {
    anchor: Anchor,
    cells: &'a [Cell],
    cursor: usize,
}

impl<'a> Observed<'a> {
    fn from_terminal(terminal: &'a Terminal) -> Option<Self> {
        let model = &terminal.model;
        if terminal.profile() != TerminalProfile::Xterm
            || model.alternate_screen
            || !model.cursor_visible
            || model.mouse.reporting()
            || model.style.hidden
            || model.protected
            || model.scroll_top != 0
            || model.scroll_bottom + 1 != model.rows
            || model.horizontal_margins
        {
            return None;
        }

        let screen = model.screen();
        let cursor = screen.cursor;
        let line = &screen.lines[cursor.row];
        let previous_wrap = if cursor.row == 0 {
            screen.history.back().and_then(|line| line.wrap_length)
        } else {
            screen.lines[cursor.row - 1].wrap_length
        };
        if screen.wrap_pending
            || previous_wrap.is_some()
            || line.wrap_length.is_some()
            || line.cells[cursor.column].width == 0
            || line
                .cells
                .iter()
                .any(|cell| cell.style.hidden && !cell.text.is_empty())
        {
            return None;
        }

        Some(Self {
            anchor: Anchor {
                row: cursor.row,
                rows: model.rows,
                columns: model.columns,
                history: screen.history.len(),
            },
            cells: &line.cells,
            cursor: cursor.column,
        })
    }

    fn copy_line(&self) -> Row {
        Row {
            cells: self.cells.to_vec(),
            cursor: self.cursor,
        }
    }

    fn matches(&self, line: &Row) -> bool {
        self.cursor == line.cursor
            && self
                .cells
                .iter()
                .zip(&line.cells)
                .all(|(actual, expected)| {
                    actual.width == expected.width
                        && normalized_text(&actual.text) == normalized_text(&expected.text)
                })
    }
}

fn normalized_text(text: &str) -> &str {
    if text.is_empty() { " " } else { text }
}

impl Default for TypeAhead {
    fn default() -> Self {
        Self::new(LATENCY_THRESHOLD, TypeAheadShell::Unknown)
    }
}

impl TypeAhead {
    pub fn new(latency_threshold: Duration, shell: TypeAheadShell) -> Self {
        Self {
            shell,
            latency_threshold,
            latencies: VecDeque::new(),
            showing: false,
            out_of_sync: false,
            last_input: None,
            confirmed: None,
            pending: VecDeque::new(),
            next_id: 0,
        }
    }

    /// Called only for direct keyboard input, before writing it. Clear the
    /// predictor on paste, broadcast, focus/context changes or known sensitive
    /// input. Returns whether a previously rendered overlay may have changed.
    pub fn on_input(&mut self, terminal: &Terminal, keyboard_bytes: &[u8], now: Instant) -> bool {
        let before = self.visible_prediction_id();
        self.expire(now);
        self.process_input(terminal, keyboard_bytes, now);
        before != self.visible_prediction_id()
    }

    /// Called after applying transport output to the real terminal. The
    /// observed row must match an exact prefix of the queued predictions.
    pub fn on_output(&mut self, terminal: &Terminal, now: Instant) -> bool {
        let before = self.visible_prediction_id();
        self.expire(now);
        self.process_output(terminal, now);
        before != self.visible_prediction_id()
    }

    /// Drops the preview and the current echo trust, retaining latency samples.
    pub fn clear(&mut self) -> bool {
        let was_visible = self.visible_prediction_id().is_some();
        self.drop_predictions();
        self.out_of_sync = false;
        self.last_input = None;
        was_visible
    }

    /// Removes stale pending input. The session should wake by deadline even
    /// when the remote endpoint sends nothing.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|deadline| deadline <= now) {
            self.clear()
        } else {
            false
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending
            .front()
            .map(|prediction| prediction.created_at + MAX_TERMINAL_DELAY)
    }

    pub fn overlay(&self) -> Option<TypeAheadOverlay<'_>> {
        let (confirmed, prediction) = self.visible_prediction()?;
        Some(TypeAheadOverlay {
            row: confirmed.anchor.row as u16,
            cursor_column: prediction.line.cursor as u16,
            cells: &prediction.line.cells,
        })
    }

    fn process_input(&mut self, terminal: &Terminal, keyboard_bytes: &[u8], now: Instant) {
        if keyboard_bytes.is_empty() {
            return;
        }
        let (Some(observed), Some(edits)) = (
            Observed::from_terminal(terminal),
            Edits::decode(keyboard_bytes),
        ) else {
            self.clear();
            return;
        };

        let recent = self
            .last_input
            .is_some_and(|last| now.saturating_duration_since(last) < self.sync_delay());
        self.last_input = Some(now);
        if self.out_of_sync && recent {
            return;
        }
        self.out_of_sync = false;
        self.update_latency_mode(recent);

        if self.confirmed.as_ref().is_none_or(|confirmed| {
            observed.anchor != confirmed.anchor || !observed.matches(&confirmed.line)
        }) {
            self.drop_predictions();
            self.confirmed = Some(Confirmed {
                anchor: observed.anchor,
                line: observed.copy_line(),
                floor: observed.cursor,
                has_character_echo: false,
            });
        }

        let confirmed = self.confirmed.as_ref().expect("observed line");
        let floor = confirmed.floor;
        let mut line = self
            .pending
            .back()
            .map_or(&confirmed.line, |prediction| &prediction.line)
            .clone();
        for edit in edits {
            if self.pending.len() == MAX_PENDING {
                self.lose_sync();
                return;
            }
            match line.apply(edit, floor, self.shell) {
                EditOutcome::Unchanged => continue,
                EditOutcome::Boundary => {
                    self.lose_sync();
                    return;
                }
                EditOutcome::Changed => {}
            }
            self.next_id = self.next_id.wrapping_add(1);
            self.pending.push_back(Prediction {
                id: self.next_id,
                created_at: now,
                edit,
                line: line.clone(),
            });
        }
    }

    fn process_output(&mut self, terminal: &Terminal, now: Instant) {
        let Some(confirmed) = &self.confirmed else {
            return;
        };
        let Some(observed) = Observed::from_terminal(terminal) else {
            self.lose_sync();
            return;
        };
        if observed.anchor != confirmed.anchor {
            self.lose_sync();
            return;
        }
        if observed.matches(&confirmed.line) {
            return;
        }
        let Some(through) = self
            .pending
            .iter()
            .position(|prediction| observed.matches(&prediction.line))
        else {
            self.lose_sync();
            return;
        };

        let echoed_character = self.pending[through].echoed_character_after(&confirmed.line);
        for prediction in self.pending.drain(..=through) {
            if self.latencies.len() == LATENCY_SAMPLES {
                self.latencies.pop_front();
            }
            self.latencies.push_back(
                now.saturating_duration_since(prediction.created_at)
                    .min(MAX_TERMINAL_DELAY),
            );
        }

        let confirmed = self.confirmed.as_mut().expect("observed line");
        let mut line = observed.copy_line();
        confirmed.line = line.clone();
        confirmed.has_character_echo |= echoed_character;
        for prediction in &mut self.pending {
            if line.apply(prediction.edit, confirmed.floor, self.shell) != EditOutcome::Changed {
                self.lose_sync();
                return;
            }
            // Keep real echo styles for accepted characters and shift them with
            // subsequent edits. Only unconfirmed glyphs use the preview color.
            prediction.line = line.clone();
            self.next_id = self.next_id.wrapping_add(1);
            prediction.id = self.next_id;
        }
    }

    fn drop_predictions(&mut self) {
        self.pending.clear();
        self.confirmed = None;
    }

    fn lose_sync(&mut self) {
        self.drop_predictions();
        self.out_of_sync = true;
    }

    fn visible_prediction(&self) -> Option<(&Confirmed, &Prediction)> {
        let confirmed = self.confirmed.as_ref()?;
        if self.showing && confirmed.has_character_echo {
            Some((confirmed, self.pending.back()?))
        } else {
            None
        }
    }

    fn visible_prediction_id(&self) -> Option<u64> {
        self.visible_prediction()
            .map(|(_, prediction)| prediction.id)
    }

    fn sync_delay(&self) -> Duration {
        if self.latencies.len() < 2 {
            MAX_TERMINAL_DELAY
        } else {
            self.latencies
                .iter()
                .copied()
                .max()
                .unwrap_or(MAX_TERMINAL_DELAY)
        }
    }

    fn update_latency_mode(&mut self, recent: bool) {
        let size = self.latencies.len();
        if size < 2 {
            return;
        }
        let mut ordered = [Duration::ZERO; LATENCY_SAMPLES];
        for (destination, &sample) in ordered.iter_mut().zip(&self.latencies) {
            *destination = sample;
        }
        ordered[..size].sort_unstable();
        let upper = ordered[size / 2];
        let median = if size.is_multiple_of(2) {
            let lower = ordered[size / 2 - 1];
            lower + (upper - lower) / 2
        } else {
            upper
        };
        if median >= self.latency_threshold {
            self.showing = true;
        } else if !recent && median < self.latency_threshold / 2 {
            self.showing = false;
        }
    }
}

impl Prediction {
    fn echoed_character_after(&self, before: &Row) -> bool {
        let Edit::Character(ch) = self.edit else {
            return false;
        };
        // An acknowledged cursor or deletion can end a batch in which a
        // previously typed glyph disappeared. It proves nothing about echo.
        let width = ch.width().unwrap_or(0);
        let column = if width == 0 {
            self.line.previous(self.line.cursor)
        } else {
            self.line.cursor - width
        };
        normalized_text(&self.line.cells[column].text)
            != normalized_text(&before.cells[column].text)
    }
}

#[derive(Clone, Copy)]
enum Edit {
    Character(char),
    Backspace,
    WordBackspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
}

enum Edits<'a> {
    Single(Option<Edit>),
    Text(std::str::Chars<'a>),
}

impl<'a> Edits<'a> {
    fn decode(bytes: &'a [u8]) -> Option<Self> {
        let key = match bytes {
            b"\x7f" => Edit::Backspace,
            b"\x1b\x7f" => Edit::WordBackspace,
            b"\x1b[3~" => Edit::Delete,
            b"\x1b[D" | b"\x1bOD" => Edit::Left,
            b"\x1b[C" | b"\x1bOC" => Edit::Right,
            b"\x1bb" | b"\x1b[1;3D" | b"\x1b[1;5D" => Edit::WordLeft,
            b"\x1bf" | b"\x1b[1;3C" | b"\x1b[1;5C" => Edit::WordRight,
            b"\x01" | b"\x1b[H" | b"\x1bOH" => Edit::Home,
            b"\x05" | b"\x1b[F" | b"\x1bOF" => Edit::End,
            _ if bytes.len() <= MAX_INPUT_BYTES => {
                let text = std::str::from_utf8(bytes).ok()?;
                return text
                    .chars()
                    .all(printable)
                    .then(|| Self::Text(text.chars()));
            }
            _ => return None,
        };
        Some(Self::Single(Some(key)))
    }
}

impl Iterator for Edits<'_> {
    type Item = Edit;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(edit) => edit.take(),
            Self::Text(chars) => chars.next().map(Edit::Character),
        }
    }
}

fn printable(ch: char) -> bool {
    !ch.is_control()
        && !is_bidi_control(ch)
        && !matches!(ch, '\u{2028}' | '\u{2029}' | '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}' | '\u{100000}'..='\u{10fffd}')
        && ch.width().is_some()
}

#[derive(Clone)]
struct Row {
    cells: Vec<Cell>,
    cursor: usize,
}

#[derive(Eq, PartialEq)]
enum EditOutcome {
    Changed,
    Unchanged,
    Boundary,
}

impl Row {
    fn apply(&mut self, edit: Edit, floor: usize, shell: TypeAheadShell) -> EditOutcome {
        if self.cursor < floor || self.cells[self.cursor].width == 0 {
            return EditOutcome::Boundary;
        }

        let end = self.text_end();
        let target = match edit {
            Edit::Character(ch) => return self.insert(ch, floor),
            Edit::Backspace => {
                let from = self.previous(self.cursor).max(floor);
                return self.remove(from, self.cursor);
            }
            Edit::WordBackspace => {
                let from = self.word_left(floor, shell);
                return self.remove(from, self.cursor);
            }
            Edit::Delete => {
                if self.cursor >= end {
                    return EditOutcome::Unchanged;
                }
                return self.remove(self.cursor, self.next(self.cursor));
            }
            Edit::Left => self.previous(self.cursor).max(floor),
            Edit::Right if self.cursor < end => self.next(self.cursor),
            Edit::Right => self.cursor,
            Edit::WordLeft => self.word_left(floor, shell),
            Edit::WordRight => self.word_right(end, shell),
            Edit::Home => floor,
            Edit::End => end.max(floor),
        };
        if target >= self.cells.len() || self.cells[target].width == 0 {
            return EditOutcome::Boundary;
        }
        if self.cursor == target {
            return EditOutcome::Unchanged;
        }
        self.cursor = target;
        EditOutcome::Changed
    }

    fn text_end(&self) -> usize {
        self.cells
            .iter()
            .rposition(|cell| !cell.text.is_empty())
            .map_or(0, |last| self.next(last))
    }

    fn previous(&self, column: usize) -> usize {
        let previous = column.saturating_sub(1);
        previous.saturating_sub(usize::from(self.cells[previous].width == 0))
    }

    fn next(&self, column: usize) -> usize {
        column + usize::from(self.cells[column].width.max(1))
    }

    fn word_left(&self, floor: usize, shell: TypeAheadShell) -> usize {
        let mut column = self.cursor;
        while column > floor && !self.is_word(self.previous(column), shell) {
            column = self.previous(column).max(floor);
        }
        while column > floor && self.is_word(self.previous(column), shell) {
            column = self.previous(column).max(floor);
        }
        column
    }

    fn word_right(&self, end: usize, shell: TypeAheadShell) -> usize {
        let mut column = self.cursor;
        let first_is_word = shell == TypeAheadShell::Zsh;
        for is_word in [first_is_word, !first_is_word] {
            while column < end && self.is_word(column, shell) == is_word {
                column = self.next(column);
            }
        }
        column
    }

    fn is_word(&self, column: usize, shell: TypeAheadShell) -> bool {
        self.cells[column].text.chars().next().is_some_and(|ch| {
            ch.is_alphanumeric() || (shell == TypeAheadShell::Zsh && ZSH_WORD_CHARS.contains(ch))
        })
    }

    fn insert(&mut self, ch: char, floor: usize) -> EditOutcome {
        let width = ch.width().unwrap_or(0);
        let style = Style {
            foreground: Color::Indexed(8),
            ..Style::default()
        };
        if width == 0 {
            let previous = self.previous(self.cursor);
            if self.cursor <= floor || previous < floor {
                return EditOutcome::Boundary;
            }
            let cell = &mut self.cells[previous];
            if cell.text.is_empty() || cell.text.len() >= MAX_GLYPH_BYTES {
                return EditOutcome::Boundary;
            }
            cell.text.push(ch);
            cell.style.foreground = style.foreground;
            if cell.width == 2 {
                self.cells[previous + 1].style.foreground = style.foreground;
            }
        } else {
            let columns = self.cells.len();
            if self.cursor + width >= columns
                || !self.cells[columns - width..]
                    .iter()
                    .all(Cell::is_default_blank)
            {
                return EditOutcome::Boundary;
            }
            let mut glyph = Cell::blank(style);
            glyph.text.push(ch);
            glyph.width = width as u8;
            self.cells.insert(self.cursor, glyph);
            if width == 2 {
                self.cells
                    .insert(self.cursor + 1, Cell::continuation(style, None, false));
            }
            self.cells.truncate(columns);
            self.cursor += width;
        }
        EditOutcome::Changed
    }

    fn remove(&mut self, start: usize, end: usize) -> EditOutcome {
        if start == end {
            return EditOutcome::Unchanged;
        }
        let columns = self.cells.len();
        self.cells.drain(start..end);
        self.cells
            .resize_with(columns, || Cell::blank(Style::default()));
        self.cursor = start;
        EditOutcome::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn seeded(shell: TypeAheadShell) -> (Terminal, TypeAhead, Instant) {
        let mut terminal = Terminal::new(100, 4);
        terminal.advance(b"$ ");
        let mut predictor = TypeAhead::new(LATENCY_THRESHOLD, shell);
        let now = Instant::now();
        for (time, byte) in [(0, b"a".as_slice()), (150, b"b".as_slice())] {
            assert!(!predictor.on_input(&terminal, byte, at(now, time)));
            assert!(predictor.overlay().is_none());
            terminal.advance(byte);
            assert!(!predictor.on_output(&terminal, at(now, time + 120)));
        }
        (terminal, predictor, at(now, 271))
    }

    fn overlay_text(predictor: &TypeAhead) -> String {
        predictor
            .overlay()
            .unwrap()
            .cells
            .iter()
            .filter(|cell| cell.width > 0)
            .map(|cell| normalized_text(&cell.text))
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn high_latency_display_and_chunked_echo_reconciliation_preserve_real_cells() {
        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Bash);
        assert!(predictor.on_input(&terminal, b"cd", now));
        assert_eq!(overlay_text(&predictor), "$ abcd");
        let overlay = predictor.overlay().unwrap();
        assert_eq!((overlay.row, overlay.cursor_column), (0, 6));
        assert_eq!(overlay.cells[4].style.foreground, Color::Indexed(8));
        assert_eq!(terminal.snapshot().cursor.column, 4);
        assert!(terminal.snapshot().row(0).unwrap()[4].text.is_empty());

        terminal.advance(b"\x1b[31mc\x1b[0m");
        assert!(predictor.on_output(&terminal, at(now, 121)));
        assert_eq!(overlay_text(&predictor), "$ abcd");
        let overlay = predictor.overlay().unwrap();
        assert_eq!(overlay.cells[4].style.foreground, Color::Indexed(1));
        assert_eq!(overlay.cells[5].style.foreground, Color::Indexed(8));
        assert_eq!(predictor.pending.len(), 1);
        terminal.advance(b"d");
        assert!(predictor.on_output(&terminal, at(now, 124)));
        assert!(predictor.overlay().is_none());
        assert!(predictor.deadline().is_none());
    }

    #[test]
    fn idle_fast_echo_turns_preview_off_after_the_median_drops() {
        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        for tick in 0..LATENCY_SAMPLES as u64 {
            let typed = at(now, 20 * tick);
            assert!(predictor.on_input(&terminal, b"x", typed));
            terminal.advance(b"x");
            predictor.on_output(&terminal, at(typed, 10));
        }
        assert_eq!(predictor.latencies.len(), LATENCY_SAMPLES);
        assert!(!predictor.on_input(&terminal, b"y", at(now, 1200)));
        assert!(predictor.overlay().is_none());
        assert_eq!(predictor.pending.len(), 1);
    }

    #[test]
    fn password_context_must_echo_a_character_before_any_preview() {
        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        terminal.advance(b"\r\nPassword: ");
        assert!(!predictor.on_output(&terminal, now));

        let typed = at(now, 200);
        assert!(!predictor.on_input(&terminal, b"secret", typed));
        assert!(predictor.overlay().is_none());
        assert!(!predictor.on_output(&terminal, at(typed, 200)));
        assert_eq!(predictor.deadline(), Some(typed + MAX_TERMINAL_DELAY));
        assert!(!predictor.expire(typed + MAX_TERMINAL_DELAY));
        assert!(predictor.pending.is_empty());

        // Echo verifies the new context. Subsequent text can be shown again.
        let resumed = at(typed, 1600);
        assert!(!predictor.on_input(&terminal, b"n", resumed));
        terminal.advance(b"n");
        assert!(!predictor.on_output(&terminal, at(resumed, 120)));
        assert!(predictor.on_input(&terminal, b"e", at(resumed, 121)));
        assert_eq!(overlay_text(&predictor), "Password: ne");
    }

    #[test]
    fn a_cursor_echo_does_not_authenticate_preceding_canceled_text() {
        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        terminal.advance(b"yy\x1b[5G");
        predictor.clear();
        assert!(!predictor.on_input(&terminal, b"x", now));
        assert!(!predictor.on_input(&terminal, b"\x7f", now));
        assert!(!predictor.on_input(&terminal, b"\x1b[C", now));

        // The endpoint acknowledges the cursor movement but never echoes x.
        terminal.advance(b"\x1b[C");
        assert!(!predictor.on_output(&terminal, at(now, 120)));
        assert!(!predictor.on_input(&terminal, b"secret", at(now, 121)));
        assert!(predictor.overlay().is_none());
    }

    #[test]
    fn concealment_alternate_buffer_mouse_and_resize_suppress_predictions() {
        for change in [
            b"\x1b[8m".as_slice(),
            b"\x1b[?1049h",
            b"\x1b[?1000h",
            b"\x1b[?25l",
            b"\x1b[2;4r",
        ] {
            let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
            assert!(predictor.on_input(&terminal, b"c", now));
            terminal.advance(change);
            assert!(predictor.on_output(&terminal, at(now, 30)));
            assert!(!predictor.on_input(&terminal, b"typing", at(now, 200)));
            assert!(predictor.overlay().is_none());
        }

        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        assert!(predictor.on_input(&terminal, b"c", now));
        terminal.resize(80, 4);
        assert!(predictor.on_output(&terminal, at(now, 30)));
        assert!(predictor.overlay().is_none());
    }

    #[test]
    fn unexpected_output_and_unsupported_input_fail_closed() {
        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        assert!(predictor.on_input(&terminal, b"c", now));
        terminal.advance(b"X");
        assert!(predictor.on_output(&terminal, at(now, 20)));
        assert!(!predictor.on_input(&terminal, b"d", at(now, 21)));
        assert!(predictor.pending.is_empty());
        assert!(!predictor.on_input(&terminal, b"e", at(now, 142)));
        assert_eq!(predictor.pending.len(), 1);
        assert!(predictor.overlay().is_none());
        assert!(!predictor.on_input(&terminal, b"paste\nline", at(now, 143)));
        assert!(predictor.pending.is_empty());
        assert!(predictor.deadline().is_none());
    }

    #[test]
    fn no_output_expiry_clears_the_preview_and_requires_fresh_echo() {
        let (terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        assert!(predictor.on_input(&terminal, b"c", now));
        assert!(!predictor.expire(at(now, 1499)));
        assert!(predictor.expire(at(now, 1500)));
        assert!(predictor.overlay().is_none());
        assert!(!predictor.on_input(&terminal, b"d", at(now, 1501)));
        assert!(predictor.overlay().is_none());
    }

    #[test]
    fn character_and_cursor_edits_keep_the_prompt_and_real_grid_unchanged() {
        let (terminal, mut predictor, now) = seeded(TypeAheadShell::Bash);
        assert!(predictor.on_input(&terminal, b" cd", now));
        assert!(predictor.on_input(&terminal, b"\x1b[D", now));
        assert!(predictor.on_input(&terminal, b"\x7f", now));
        assert_eq!(overlay_text(&predictor), "$ ab d");
        assert!(predictor.on_input(&terminal, b"\x1b[3~", now));
        assert_eq!(overlay_text(&predictor), "$ ab");
        assert!(predictor.on_input(&terminal, b"\x01", now));
        assert_eq!(predictor.overlay().unwrap().cursor_column, 2);
        assert!(!predictor.on_input(&terminal, b"\x7f", now));
        assert!(!predictor.on_input(&terminal, b"\x1b[D", now));
        assert!(predictor.on_input(&terminal, b"\x05", now));
        assert_eq!(predictor.overlay().unwrap().cursor_column, 5);
        assert_eq!(terminal.snapshot().cursor.column, 4);
    }

    #[test]
    fn shell_word_movement_matches_bash_and_zsh_boundaries() {
        for (shell, destination) in [(TypeAheadShell::Bash, 4), (TypeAheadShell::Zsh, 8)] {
            let (terminal, mut predictor, now) = seeded(shell);
            assert!(predictor.on_input(&terminal, b"-cd xx", now));
            assert!(predictor.on_input(&terminal, b"\x1b[H", now));
            assert!(predictor.on_input(&terminal, b"\x1bf", now));
            assert_eq!(predictor.overlay().unwrap().cursor_column, destination);
            assert!(predictor.on_input(&terminal, b"\x1b[F", now));
            assert!(predictor.on_input(&terminal, b"\x1b\x7f", now));
            assert_eq!(overlay_text(&predictor), "$ ab-cd");
            assert_eq!(predictor.overlay().unwrap().cursor_column, 8);
        }
    }

    #[test]
    fn wide_and_combining_cells_remain_consistent_during_editing() {
        let (terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        assert!(predictor.on_input(&terminal, "界e\u{301}".as_bytes(), now));
        let overlay = predictor.overlay().unwrap();
        assert_eq!(overlay.cursor_column, 7);
        assert_eq!(overlay.cells[4].width, 2);
        assert_eq!(overlay.cells[5].width, 0);
        assert_eq!(overlay.cells[6].text, "e\u{301}");
        assert!(predictor.on_input(&terminal, b"\x7f", now));
        assert_eq!(predictor.overlay().unwrap().cursor_column, 6);
        assert!(predictor.on_input(&terminal, b"\x7f", now));
        assert_eq!(overlay_text(&predictor), "$ ab");
        assert_eq!(predictor.overlay().unwrap().cursor_column, 4);
    }

    #[test]
    fn pending_and_right_edge_limits_drop_unsafe_previews() {
        let (mut terminal, mut predictor, now) = seeded(TypeAheadShell::Unknown);
        assert!(predictor.on_input(&terminal, b"c", now));
        assert!(predictor.on_input(&terminal, &[b'x'; MAX_PENDING], now));
        assert!(predictor.overlay().is_none());
        assert!(predictor.pending.is_empty());

        predictor.clear();
        terminal.advance(b"\x1b[99G");
        assert!(!predictor.on_input(&terminal, b"c", now));
        assert_eq!(predictor.pending.len(), 1);
        assert!(!predictor.on_input(&terminal, b"d", now));
        assert!(predictor.pending.is_empty());
        assert!(predictor.overlay().is_none());
    }
}
