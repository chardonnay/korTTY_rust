//! Text-only rendering of the reviewed automation journal viewport.

use std::time::{Duration, Instant};

use kortty_recording::{RecordedCell, RecordedScreen};
use kortty_terminal::{Cell, Cursor, CursorShape, ScreenSnapshot, Style, Terminal};
use zeroize::{Zeroize as _, Zeroizing};

use super::Options;
use crate::recording_window::render;
use crate::session_journal::{MAX_RECORD_BYTES, is_display_character};
use crate::terminal_logging::Redactor;

const REDACTION_BUDGET: Duration = Duration::from_millis(100);
const MAX_CONTEXT_BYTES: usize = 512 * 1024;

pub(super) fn render(terminal: &Terminal, redactor: &Redactor, drained: bool) -> Option<Vec<u8>> {
    let deadline = Instant::now() + REDACTION_BUDGET;
    let screen = terminal.snapshot();
    let safe = masked_screen(terminal, &screen, redactor, drained, deadline)?;
    render::png(&safe).ok()
}

fn masked_screen(
    terminal: &Terminal,
    screen: &ScreenSnapshot,
    redactor: &Redactor,
    drained: bool,
    deadline: Instant,
) -> Option<RecordedScreen> {
    let columns = usize::from(screen.columns);
    if (Options {
        columns: screen.columns,
        rows: screen.rows,
        ..Options::default()
    })
    .validate()
    .is_err()
        || screen.cells.len() != columns * usize::from(screen.rows)
    {
        return None;
    }

    let has_rules = redactor.redaction_policy().has_redactions();
    let history = screen.scrollback_lines;
    let mut first = history;
    if has_rules {
        // The whole-screen check includes preceding retained context for multiline rules.
        first = 0;
        if terminal.history_starts_with_continuation() {
            return None;
        }
    } else {
        while first > 0 && terminal.history_wrapped_columns(first - 1).is_some() {
            if Instant::now() >= deadline {
                return None;
            }
            first -= 1;
        }
    }

    let mut line = LogicalLine::default();
    let mut visible_context = Contexts::default();
    let mut hidden_context = Contexts::default();
    for row in first..history {
        if Instant::now() >= deadline {
            return None;
        }
        let cells = terminal.history_line(row)?;
        let wrap = terminal.history_wrapped_columns(row);
        line.extend(&cells, wrap)?;
        if wrap.is_none() {
            debug_assert!(has_rules);
            visible_context.add(line.visible(), redactor, deadline)?;
            hidden_context.add(line.with_hidden(), redactor, deadline)?;
            line.clear();
        }
    }

    let mut output = Vec::with_capacity(screen.cells.len());
    let mut start = 0;
    let mut incomplete = first == 0 && terminal.history_starts_with_continuation()
        || screen.starts_with_continuation() && first == history;
    for row in 0..screen.rows {
        if Instant::now() >= deadline {
            return None;
        }
        let end = (usize::from(row) + 1) * columns;
        let cells = &screen.cells[end - columns..end];
        let wrap = screen.wrapped_columns(row).map(usize::from);
        line.extend(cells, wrap)?;
        if wrap.is_some() && row + 1 < screen.rows {
            continue;
        }
        let visible = line.visible();
        let with_hidden = line.with_hidden();
        let same = if has_rules {
            let visible_clear = visible_context.add(visible, redactor, deadline)?;
            let hidden_clear = hidden_context.add(with_hidden, redactor, deadline)?;
            visible_clear && hidden_clear
        } else {
            !redactor.contains_known_secret(visible)
                && (with_hidden == visible || !redactor.contains_known_secret(with_hidden))
        };
        let cursor_row = usize::from(screen.cursor.row);
        let cursor_in_line = (start / columns..=usize::from(row)).contains(&cursor_row);
        let partial_secret = !drained
            && cursor_in_line
            && (has_rules
                || redactor.ends_with_partial_secret(visible, deadline)?
                || with_hidden != visible
                    && redactor.ends_with_partial_secret(with_hidden, deadline)?);
        let omit = !same || incomplete || wrap.is_some() || partial_secret;
        for row in (start / columns)..=usize::from(row) {
            let offset = row * columns;
            let wrap = screen
                .wrapped_columns(row as u16)
                .map(usize::from)
                .unwrap_or(columns);
            for (column, cell) in screen.cells[offset..offset + columns].iter().enumerate() {
                output.push(masked_cell(cell, omit || column >= wrap));
            }
        }
        start = end;
        incomplete = false;
        line.clear();
    }

    if has_rules
        && (!visible_context.checked(redactor, deadline)?
            || visible_context.explicit.raw != hidden_context.explicit.raw
                && !hidden_context.checked(redactor, deadline)?)
    {
        // A rule spanning hard line breaks may mask cells on both sides of a boundary.
        // If its result differs from isolated line redactions, withhold the frame.
        return None;
    }
    (Instant::now() < deadline).then_some(RecordedScreen {
        columns: screen.columns,
        rows: screen.rows,
        pixels: (0, 0),
        cursor: Cursor::default(),
        cursor_shape: CursorShape::default(),
        cursor_blinking: false,
        reverse_video: screen.reverse_video,
        cells: output,
    })
}

fn masked_cell(cell: &Cell, omit: bool) -> RecordedCell {
    RecordedCell {
        text: if omit || cell.style.hidden || cell.width == 0 {
            String::new()
        } else {
            cell.text
                .chars()
                .filter(|&character| is_cell_character(character))
                .collect()
        },
        width: if omit { 1 } else { cell.width },
        style: if omit { Style::default() } else { cell.style },
    }
}

#[derive(Default)]
struct LogicalLine {
    visible: Zeroizing<String>,
    with_hidden: Zeroizing<String>,
    visible_end: usize,
    with_hidden_end: usize,
}

impl LogicalLine {
    fn extend(&mut self, cells: &[Cell], wrap: Option<usize>) -> Option<()> {
        let cells = cells.get(..wrap.unwrap_or(cells.len()))?;
        let mut remaining = 0;
        for cell in cells {
            match cell.width {
                0 if remaining == 1 => {
                    remaining = 0;
                    continue;
                }
                1 | 2 if remaining == 0 => remaining = cell.width - 1,
                _ => return None,
            }
            let spare = usize::from(cell.width);
            let bytes = cell.text.len().max(spare);
            if self.visible.len().max(self.with_hidden.len()) + bytes > MAX_RECORD_BYTES {
                return None;
            }
            let characters = cell
                .text
                .chars()
                .filter(|&character| is_cell_character(character));
            let previously = self.with_hidden.len();
            self.with_hidden.extend(characters);
            let explicit = self.with_hidden.len() != previously;
            if explicit {
                self.with_hidden_end = self.with_hidden.len();
            } else {
                self.with_hidden.extend(std::iter::repeat_n(' ', spare));
            }
            if cell.style.hidden {
                self.visible.extend(std::iter::repeat_n(' ', spare));
            } else {
                self.visible.push_str(&self.with_hidden[previously..]);
                if explicit {
                    self.visible_end = self.visible.len();
                }
            }
        }
        if wrap.is_some() {
            self.visible_end = self.visible.len();
            self.with_hidden_end = self.with_hidden.len();
        }
        (remaining == 0).then_some(())
    }

    fn visible(&self) -> &str {
        &self.visible[..self.visible_end]
    }

    fn with_hidden(&self) -> &str {
        &self.with_hidden[..self.with_hidden_end]
    }

    fn clear(&mut self) {
        self.visible.zeroize();
        self.with_hidden.zeroize();
        self.visible_end = 0;
        self.with_hidden_end = 0;
    }
}

fn is_cell_character(character: char) -> bool {
    !character.is_control() && is_display_character(character)
}

#[derive(Default)]
struct Context {
    raw: Zeroizing<String>,
    masked: Zeroizing<String>,
    replacements: usize,
}

#[derive(Default)]
struct Contexts {
    explicit: Context,
    trimmed: Context,
}

impl Contexts {
    fn add(&mut self, text: &str, redactor: &Redactor, deadline: Instant) -> Option<bool> {
        let (masked, count) = redactor.outbound_text(text, deadline)?;
        self.explicit.add(text, &masked, count)?;
        let explicit_clear = count == 0 && masked.as_str() == text;
        let trimmed = text.trim_end();
        let (trimmed_masked, trimmed_count) = if trimmed == text {
            (masked, count)
        } else {
            redactor.outbound_text(trimmed, deadline)?
        };
        self.trimmed.add(trimmed, &trimmed_masked, trimmed_count)?;
        Some(explicit_clear && trimmed_count == 0 && trimmed_masked.as_str() == trimmed)
    }

    fn checked(&self, redactor: &Redactor, deadline: Instant) -> Option<bool> {
        Some(
            self.explicit.checked(redactor, deadline)?
                && (self.explicit.raw == self.trimmed.raw
                    || self.trimmed.checked(redactor, deadline)?),
        )
    }
}

impl Context {
    fn add(&mut self, text: &str, masked: &str, replaced: usize) -> Option<()> {
        append(&mut self.raw, text)?;
        append(&mut self.masked, masked)?;
        self.replacements += replaced;
        Some(())
    }

    fn checked(&self, redactor: &Redactor, deadline: Instant) -> Option<bool> {
        let (masked, replaced) = redactor.outbound_text(&self.raw, deadline)?;
        Some(masked == self.masked && replaced == self.replacements)
    }
}

fn append(target: &mut String, text: &str) -> Option<()> {
    if target.len() + text.len() + 1 > MAX_CONTEXT_BYTES {
        return None;
    }
    target.push_str(text);
    target.push('\n');
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    fn redact(secret: &str, rule: Option<&str>) -> Redactor {
        let secrets = Secrets::default();
        secrets.add(secret);
        let mut policy = Policy::default();
        if let Some(rule) = rule {
            let document: toml_edit::DocumentMut =
                format!("[[session-journal.replace]]\npattern = '{rule}'\nregex = true\n")
                    .parse()
                    .unwrap();
            policy.read_rule(document.as_table()).unwrap();
        }
        Redactor::for_journal(secrets.seal().unwrap(), policy)
    }

    fn masked(terminal: &Terminal, redactor: &Redactor, drained: bool) -> Option<RecordedScreen> {
        masked_screen(
            terminal,
            &terminal.snapshot(),
            redactor,
            drained,
            Instant::now() + Duration::from_secs(3),
        )
    }

    fn texts(screen: &RecordedScreen, row: u16) -> String {
        screen
            .row(row)
            .unwrap()
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn journal_viewport_omits_soft_wrapped_secrets_without_moving_unaffected_cells() {
        let secret = "nonAscii-界e\u{301}-secret";
        let mut terminal = Terminal::new(40, 10);
        // A two-cell glyph at column 39 wraps early, leaving a physical padding cell.
        terminal.advance(
            format!(
                "{}nonAscii-界e\u{301}-secret\r\nplain\t\x1b[8mhidden\x1b[28m界e\u{301}",
                ".".repeat(30)
            )
            .as_bytes(),
        );
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.wrapped_columns(0), Some(39));
        let redactor = redact(secret, None);
        let screen = masked(&terminal, &redactor, true).unwrap();
        assert_eq!(texts(&screen, 0), "");
        assert_eq!(texts(&screen, 1), "");
        assert_eq!(texts(&screen, 2), "plain   界e\u{301}");
        let row = screen.row(2).unwrap();
        assert!(row[5..8].iter().all(|cell| cell.text == " "));
        assert!(row[8..14].iter().all(|cell| cell.text.is_empty()));
        assert_eq!(row[14].text, "界");
        assert_eq!(row[14].width, 2);
        assert_eq!(row[15].width, 0);
        assert_eq!(row[16].text, "e\u{301}");
        assert!(!screen.cursor.visible);
        let png = render(&terminal, &redactor, true).unwrap();
        assert!(tiny_skia::Pixmap::decode_png(&png).is_ok());
    }

    #[test]
    fn journal_viewport_checks_the_scrolled_off_start_of_a_visible_logical_line() {
        let mut terminal = Terminal::new(40, 10);
        let secret = "start-credential-end";
        terminal.advance(
            format!("{}{}\r\n{}tail", ".".repeat(36), secret, "\r\n".repeat(8)).as_bytes(),
        );
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.scrollback_lines, 1);
        assert!(snapshot.starts_with_continuation());
        let screen = masked(&terminal, &redact(secret, None), true).unwrap();
        assert_eq!(texts(&screen, 0), "");
        assert_eq!(texts(&screen, 9), "tail");
    }

    #[test]
    fn journal_viewport_omits_clipped_alternate_screen_logical_lines() {
        let mut terminal = Terminal::new(40, 10);
        terminal.advance(b"\x1b[?1049h");
        terminal.advance("x".repeat(404).as_bytes());
        assert!(terminal.snapshot().starts_with_continuation());
        assert_eq!(terminal.snapshot().scrollback_lines, 0);
        let screen = masked(&terminal, &redact("", None), true).unwrap();
        assert!(screen.cells.iter().all(|cell| cell.text.is_empty()));
    }

    #[test]
    fn journal_viewport_masks_known_partial_secrets_until_output_is_drained() {
        let mut terminal = Terminal::new(40, 10);
        terminal.advance(b"stable\r\nprivate-token-");
        let redactor = redact("private-token-tail", None);
        assert_eq!(
            texts(&masked(&terminal, &redactor, false).unwrap(), 0),
            "stable"
        );
        assert_eq!(texts(&masked(&terminal, &redactor, false).unwrap(), 1), "");
        assert_eq!(
            texts(&masked(&terminal, &redactor, true).unwrap(), 1),
            "private-token-"
        );
        terminal.advance(b"tail");
        assert_eq!(texts(&masked(&terminal, &redactor, true).unwrap(), 1), "");
    }

    #[test]
    fn journal_viewport_withholds_multiline_redactions_and_deadline_failures() {
        let mut terminal = Terminal::new(40, 10);
        terminal.advance(b"header\r\napi token\r\nvalue\r\nfooter");
        let redactor = redact("", Some(r"api token\nvalue"));
        assert!(masked(&terminal, &redactor, true).is_none());
        let redactor = redact("", Some("api token"));
        let screen = masked(&terminal, &redactor, true).unwrap();
        assert_eq!(texts(&screen, 1), "");
        assert_eq!(texts(&screen, 2), "value");
        assert!(
            masked_screen(
                &terminal,
                &terminal.snapshot(),
                &redactor,
                true,
                Instant::now()
            )
            .is_none()
        );
    }

    #[test]
    fn journal_viewport_omits_the_live_cursor_line_when_managed_rules_are_configured() {
        let mut terminal = Terminal::new(40, 10);
        terminal.advance(b"stable\r\ntoken=prefix");
        let redactor = redact("", Some("token=prefix-and-rest"));
        let live = masked(&terminal, &redactor, false).unwrap();
        assert_eq!(texts(&live, 0), "stable");
        assert_eq!(texts(&live, 1), "");
        let final_screen = masked(&terminal, &redactor, true).unwrap();
        assert_eq!(texts(&final_screen, 1), "token=prefix");
    }

    #[test]
    fn journal_viewport_checks_explicit_terminal_spaces_and_trimmed_line_rules() {
        let mut terminal = Terminal::new(40, 10);
        terminal.advance(b"pass  \r\napi   \r\nvisible");
        let redactor = redact("", Some(r"(?m)^pass$|^api {3}$"));
        let screen = masked(&terminal, &redactor, true).unwrap();
        assert_eq!(texts(&screen, 0), "");
        assert_eq!(texts(&screen, 1), "");
        assert_eq!(texts(&screen, 2), "visible");
    }
}
