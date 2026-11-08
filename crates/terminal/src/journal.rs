//! Optional observation of parsed primary-screen text for session journals.

use std::borrow::Cow;

use crate::{Cell, Input, Model, Terminal};

/// An output action accepted by the real terminal parser. No control-string payload is exposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalEvent {
    Print(char),
    LineFeed,
    CarriageReturn,
    Backspace,
    Tab,
    /// A screen transition or reset invalidates unfinished output.
    Boundary,
}

/// A terminal-owned observer. Implementations must keep callbacks bounded and nonblocking.
pub trait JournalObserver: Send {
    fn begin_chunk(&mut self);
    fn observe(&mut self, event: JournalEvent);
    fn finish(&mut self);
}

/// A rendered history or primary-screen row, with the extent of a soft wrap if present.
pub struct JournalRow<'a> {
    pub cells: Cow<'a, [Cell]>,
    pub wrapped_columns: Option<usize>,
}

impl Terminal {
    pub fn supports_journal_output(&self) -> bool {
        matches!(self.input, Input::Xterm(_))
    }

    /// Replaces the current observer and lets it flush a final partial line.
    pub fn set_journal_observer(&mut self, observer: Option<Box<dyn JournalObserver>>) {
        if let Some(mut previous) = std::mem::replace(&mut self.model.journal, observer) {
            previous.finish();
        }
    }

    /// Rendered history followed by completed live rows. Reconnect archives, the unfinished
    /// cursor line and temporary alternate screens are excluded. Packed rows decode lazily.
    pub fn journal_rows(
        &self,
    ) -> impl DoubleEndedIterator<Item = JournalRow<'_>> + ExactSizeIterator {
        let screen = &self.model.primary;
        let first = screen.live_history_start();
        let history_end = screen.history.len();
        let mut end = if self.model.alternate_screen {
            first
        } else {
            history_end + screen.cursor.row
        };
        while end > first && self.model.journal_wrapped_at(end - 1).is_some() {
            end -= 1;
        }
        (first..end).map(move |row| {
            if row < history_end {
                let line = &screen.history[row];
                JournalRow {
                    cells: line.cells(),
                    wrapped_columns: line.wrap_length,
                }
            } else {
                let line = &screen.lines[row - history_end];
                JournalRow {
                    cells: Cow::Borrowed(&line.cells),
                    wrapped_columns: line.wrap_length,
                }
            }
        })
    }

    /// A retroactive attachment must wait before capturing a suffix of the current line.
    pub fn journal_at_line_start(&self) -> bool {
        let screen = &self.model.primary;
        let row = screen.history.len() + screen.cursor.row;
        !self.model.alternate_screen
            && screen.cursor.column == 0
            && !screen.wrap_pending
            && row
                .checked_sub(1)
                .is_none_or(|row| self.model.journal_wrapped_at(row).is_none())
    }
}

impl Model {
    fn journal_wrapped_at(&self, row: usize) -> Option<usize> {
        let screen = &self.primary;
        if row < screen.history.len() {
            screen.history[row].wrap_length
        } else {
            screen.lines[row - screen.history.len()].wrap_length
        }
    }

    pub(crate) fn journal_boundary(&mut self) {
        if let Some(observer) = &mut self.journal {
            observer.observe(JournalEvent::Boundary);
        }
    }

    pub(crate) fn journal_event(&mut self, event: JournalEvent) {
        if !self.alternate_screen
            && let Some(observer) = &mut self.journal
        {
            observer.observe(event);
        }
    }

    pub(crate) fn journal_print(&mut self, character: char) {
        if !self.style.hidden && !character.is_control() {
            self.journal_event(JournalEvent::Print(character));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    // Test double for the observer only. Every byte goes through the real terminal parser.
    struct Probe(Arc<Mutex<Vec<JournalEvent>>>);

    impl JournalObserver for Probe {
        fn begin_chunk(&mut self) {}

        fn observe(&mut self, event: JournalEvent) {
            self.0.lock().unwrap().push(event);
        }

        fn finish(&mut self) {}
    }

    #[test]
    fn parsed_journal_observer_cannot_see_opaque_or_hidden_output() {
        let mut terminal = Terminal::new(80, 8);
        let events = Arc::new(Mutex::new(Vec::new()));
        terminal.advance(b"\x9d52;c;before-attach");
        terminal.set_journal_observer(Some(Box::new(Probe(Arc::clone(&events)))));
        for chunk in [
            b"opaque\x1b[31mstill-opaque\x1b".as_slice(),
            b"\\A\x1b[?1049hALT\x1b[?1049lB\x1b[8mHIDDEN\x1b[28m",
            b"\xc2",
            b"\x9fG,APC\xc2",
            b"\x9cC\r\n",
        ] {
            terminal.advance(chunk);
        }
        terminal.echo_serial_input(b"local-echo\r\n");
        let events = events.lock().unwrap();
        let text: String = events
            .iter()
            .filter_map(|event| match event {
                JournalEvent::Print(character) => Some(*character),
                _ => None,
            })
            .collect();
        assert_eq!(text, "ABC");
        assert_eq!(
            events
                .iter()
                .filter(|&&event| event == JournalEvent::Boundary)
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|&&event| event == JournalEvent::LineFeed)
                .count(),
            1
        );
    }

    #[test]
    fn seed_rows_exclude_reconnect_archives_and_alternate_screen() {
        let mut previous = Terminal::new(16, 4);
        previous.advance(b"old-password\r\n");
        let mut terminal = Terminal::new(16, 4);
        terminal.advance(b"new-output\r\n");
        terminal.prepend_reconnect_history(previous.reconnect_history());
        let rows: Vec<String> = terminal
            .journal_rows()
            .map(|row| row.cells.iter().map(|cell| cell.text.as_str()).collect())
            .collect();
        assert!(rows.iter().any(|row| row.contains("new-output")));
        assert!(!rows.iter().any(|row| row.contains("old-password")));
        terminal.advance(b"\x1b[?1049hsecret\r\n");
        assert_eq!(terminal.journal_rows().count(), 0);
    }
}
