//! Optional observation of parsed primary-screen text for session journals.

use std::borrow::Cow;

use crate::{Cell, Model, Terminal};

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
    pub fn journal_input_encoding(&self) -> crate::TextEncoding {
        self.model.text_encoding()
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

    pub(crate) fn put_journaled_char(&mut self, character: char) {
        if self.journal.is_some() {
            self.journal_print(character);
        }
        self.put_char(character);
    }

    pub(crate) fn journal_control(&mut self, byte: u8) {
        if self.journal.is_none() {
            return;
        }
        let event = match byte {
            b'\x08' => JournalEvent::Backspace,
            b'\t' => JournalEvent::Tab,
            b'\n' | b'\x0b' | b'\x0c' => JournalEvent::LineFeed,
            b'\r' => JournalEvent::CarriageReturn,
            _ => return,
        };
        self.journal_event(event);
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
    fn petscii_journal_decodes_glyphs_and_native_line_controls() {
        use JournalEvent::{Backspace, Boundary, LineFeed, Print};

        let events = parsed_events(
            crate::TerminalProfile::Petscii,
            &[b"A\\Z\x14\r", b"\x0eAB\r\x93C\r"],
        );
        assert_eq!(
            events,
            [
                Print('A'),
                Print('£'),
                Print('Z'),
                Backspace,
                LineFeed,
                Print('a'),
                Print('b'),
                LineFeed,
                Boundary,
                Print('c'),
                LineFeed,
            ]
        );
    }

    #[test]
    fn wyse_journal_excludes_control_payloads_and_concealed_text() {
        use crate::TerminalProfile::{Wy50, Wy60, Wy160};

        for profile in [Wy50, Wy60, Wy160] {
            let events = parsed_events(
                profile,
                &[
                    b"A\x1bG1HIDDEN\x1bG0\x1bz0STATUS",
                    b"PAYLOAD\r\x1bA0KEYDEFINITION\rB\xc3",
                    b"\xa9\x1b]OPAQUE\x1b",
                    b"\\C\r\n",
                ],
            );
            assert_stream_line(&events, "ABéC");
        }
    }

    #[test]
    fn televideo_journal_excludes_status_and_key_definitions() {
        use crate::TerminalProfile::{Tvi910, Tvi920, Tvi925};

        for profile in [Tvi910, Tvi920, Tvi925] {
            let mut chunks = vec![b"A\x1bG1HIDDEN\x1bG0\x1blSTATUS".as_slice(), b"PAYLOAD\r"];
            if profile != Tvi910 {
                chunks.extend([b"\x1bpKEYDEF\x1b".as_slice(), b"\\"]);
            }
            chunks.extend([b"B\xc3".as_slice(), b"\xa9C\r\n"]);
            assert_stream_line(&parsed_events(profile, &chunks), "ABéC");
        }
    }

    #[test]
    fn hp_journal_excludes_softkey_definitions_and_concealed_text() {
        use crate::TerminalProfile::{Hp2392, Hp70092};

        for profile in [Hp2392, Hp70092] {
            let events = parsed_events(
                profile,
                &[
                    b"A\x1b&f1a6LKEY",
                    b"DEF\x1b&dSHIDDEN\x1b&d@B\xc3",
                    b"\xa9\x1b]OPAQUE\x1b",
                    b"\\C\r\n",
                ],
            );
            assert_stream_line(&events, "ABéC");
        }
    }

    #[test]
    fn hp_journal_excludes_local_form_edits_and_replies() {
        use crate::{Hp2392Input, TerminalProfile};

        let mut terminal = Terminal::with_profile(24, 4, TerminalProfile::Hp2392);
        let events = Arc::new(Mutex::new(Vec::new()));
        terminal.set_journal_observer(Some(Box::new(Probe(Arc::clone(&events)))));
        terminal.advance(b"\x1b&k1B\x1b&s1HNAME:\x1b[      \x1b] READY\x1bW");
        events.lock().unwrap().clear();
        assert!(terminal.hp2392_local_input(b"LOCAL", Hp2392Input::Text));
        let screen = terminal.snapshot();
        let text: String = screen.cells.iter().map(|cell| cell.text.as_str()).collect();
        assert!(text.contains("NAME:LOCAL"));
        terminal.advance(b"\x1bW");
        assert!(terminal.hp2392_local_input(b"\r", Hp2392Input::Key { enter: true }));
        assert!(terminal.take_responses().starts_with(b"LOCAL"));
        assert!(events.lock().unwrap().is_empty());
    }

    fn parsed_events(profile: crate::TerminalProfile, chunks: &[&[u8]]) -> Vec<JournalEvent> {
        let mut terminal = Terminal::with_profile(80, 8, profile);
        let events = Arc::new(Mutex::new(Vec::new()));
        terminal.set_journal_observer(Some(Box::new(Probe(Arc::clone(&events)))));
        for bytes in chunks {
            terminal.advance(bytes);
        }
        terminal.echo_serial_input(b"LOCAL ECHO\r\n");
        terminal.set_journal_observer(None);
        Arc::try_unwrap(events).unwrap().into_inner().unwrap()
    }

    fn assert_stream_line(events: &[JournalEvent], expected: &str) {
        let mut expected: Vec<_> = expected.chars().map(JournalEvent::Print).collect();
        expected.extend([JournalEvent::CarriageReturn, JournalEvent::LineFeed]);
        assert_eq!(events, expected);
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
