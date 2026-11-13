//! Assembly for successful physical keyboard writes at verified shell prompts.

use zeroize::{Zeroize as _, Zeroizing};

use crate::session_journal::{MAX_RECORD_BYTES, is_display_character};

#[derive(Default)]
pub(super) struct Lines {
    line: Zeroizing<String>,
    uncertain: bool,
}

impl Lines {
    pub(super) fn reset(&mut self) {
        self.line.zeroize();
        self.uncertain = false;
    }

    pub(super) fn suppress(&mut self) {
        self.line.zeroize();
        self.uncertain = true;
    }

    pub(super) fn written(&mut self, bytes: &[u8], enter: bool) -> Option<Zeroizing<String>> {
        if enter {
            let certain = !self.uncertain && matches!(bytes, b"\r" | b"\n" | b"\r\n");
            let line = std::mem::take(&mut self.line);
            self.reset();
            return (certain && !line.trim().is_empty()).then_some(line);
        }
        match bytes {
            [0x03] => self.reset(),
            [0x15] if !self.uncertain => self.reset(),
            [0x08 | 0x7f] if !self.uncertain => {
                self.line.pop();
            }
            _ if self.uncertain => {}
            _ => {
                let text = std::str::from_utf8(bytes).ok().filter(|text| {
                    self.line.len() + text.len() <= MAX_RECORD_BYTES
                        && text
                            .chars()
                            .all(|ch| !ch.is_control() && is_display_character(ch))
                });
                if let Some(text) = text {
                    self.line.push_str(text);
                } else {
                    // Completion, history navigation or unsupported editing changed the line.
                    self.suppress();
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_publish_only_submitted_physical_text_and_apply_backspace() {
        let mut lines = Lines::default();
        assert!(lines.written("echo ä".as_bytes(), false).is_none());
        assert!(lines.written(b"\x7f", false).is_none());
        assert!(lines.written("β".as_bytes(), false).is_none());
        assert_eq!(lines.written(b"\r", true).unwrap().as_str(), "echo β");
        assert!(lines.written(b"\r", true).is_none());
        lines.written(b"echo CRLF", false);
        assert_eq!(lines.written(b"\r\n", true).unwrap().as_str(), "echo CRLF");
    }

    #[test]
    fn unsupported_edits_and_suppression_clear_partial_text() {
        let mut lines = Lines::default();
        lines.written(b"discard", false);
        lines.written(b"\x1b[A", false);
        lines.written(b"unknown", false);
        assert!(lines.written(b"\r", true).is_none());
        lines.written(b"sensitive", false);
        lines.suppress();
        lines.written(b"continuation", false);
        assert!(lines.written(b"\r", true).is_none());
        lines.reset();
        lines.written(b"new command", false);
        assert_eq!(lines.written(b"\r", true).unwrap().as_str(), "new command");
    }
}
