use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::Zeroizing;

use crate::session_journal::entry_edit::MarkerSource;
use crate::session_journal::summary_ai::{Card, State};
use crate::session_journal::summary_plan::{End, Window};

use super::{Entry, MAX_TIMELINE, MAX_TIMELINE_TEXT_BYTES, Writer, element, invalid};

pub(super) const MAX_LIVE_CARDS: usize = 512;

pub(crate) struct Publish {
    pub(crate) after: u64,
    pub(crate) window: Window,
    pub(crate) card: Option<Card>,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) authorize: Option<crate::session_journal::summary_ai::Authorization>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Checkpoint {
    cursor: u64,
    last_ai: u64,
    raw: bool,
}

impl Checkpoint {
    pub(super) fn write(self, xml: &mut String) {
        if self.cursor == 0 && !self.raw {
            return;
        }
        element(xml, "nativeSummaryCursor", &self.cursor.to_string());
        element(xml, "lastSummarizedSeq", &self.last_ai.to_string());
        element(
            xml,
            "nativeSummaryRawFallback",
            if self.raw { "true" } else { "false" },
        );
    }
}

#[derive(Clone)]
pub(super) struct SummaryEntry {
    first: u64,
    last: u64,
    end: End,
    state: State,
    input: Vec<String>,
    output: Vec<String>,
}

impl SummaryEntry {
    fn from_card(window: &Window, card: &Card) -> Self {
        Self {
            first: window.first,
            last: window.last,
            end: window.end,
            state: card.state,
            input: card.input.clone(),
            output: card.output.clone(),
        }
    }

    pub(super) fn write(&self, xml: &mut String) {
        element(
            xml,
            "state",
            match self.state {
                State::Raw => "RAW",
                State::Summarized => "SUMMARIZED",
            },
        );
        if self.end != End::Complete {
            element(
                xml,
                "commandState",
                if self.end == End::Running {
                    "RUNNING"
                } else {
                    "ENDED_WITH_SESSION"
                },
            );
        }
        element(xml, "logStartSeq", &self.first.to_string());
        element(xml, "logEndSeq", &self.last.to_string());
        for (wrapper, lines) in [
            ("inputExcerpt", &self.input),
            ("outputExcerpt", &self.output),
        ] {
            if lines.is_empty() {
                continue;
            }
            use std::fmt::Write as _;
            let _ = write!(xml, "<{wrapper}>");
            for line in lines {
                element(xml, "line", line);
            }
            let _ = write!(xml, "</{wrapper}>");
        }
    }
}

impl Writer {
    pub(super) fn publish_summary(&mut self, update: Publish) -> io::Result<u64> {
        let require_active = || {
            crate::session_journal::summary_ai::authorize_publication(update.authorize.as_ref())?;
            if update.cancelled.load(Ordering::Acquire) {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Journal summary cancelled",
                ))
            } else {
                Ok(())
            }
        };
        require_active()?;
        if self.document.summary.cursor != update.after
            || update.window.first <= update.after
            || update.window.last < update.window.first
            || update.window.last > self.sequence
            || update.window.skipped != update.card.is_none()
            || update.window.skipped && update.window.end == End::Running
        {
            return Err(invalid(
                "The live journal changed before publishing its summary",
            ));
        }
        let index = self.document.entries.iter().position(|entry| {
            entry
                .summary
                .as_ref()
                .is_some_and(|old| old.first == update.window.first && old.end == End::Running)
        });
        if update.card.is_some()
            && index.is_none()
            && (self.document.entries.len() >= MAX_TIMELINE
                || self
                    .document
                    .entries
                    .iter()
                    .filter(|entry| entry.summary.is_some())
                    .count()
                    >= MAX_LIVE_CARDS)
        {
            return Err(invalid("The live journal reached its summary card limit"));
        }
        let original = index.map(|index| &self.document.entries[index]);
        let replacement = replacement_entry(&update, original)?;
        let removed_bytes = original.map_or(0, |entry| entry.fields.byte_len());
        let added_bytes = replacement
            .as_ref()
            .map_or(0, |entry| entry.fields.byte_len());
        let text_bytes = self
            .timeline_text_bytes
            .saturating_sub(removed_bytes)
            .saturating_add(added_bytes);
        if text_bytes > MAX_TIMELINE_TEXT_BYTES {
            return Err(invalid("The live journal summary text limit was reached"));
        }
        self.log.sync()?;
        let previous = index.map(|index| self.document.entries.remove(index));
        let position = index.unwrap_or(self.document.entries.len());
        let inserted = replacement.is_some();
        if let Some(entry) = replacement {
            self.document.entries.insert(position, entry);
        }
        let old_checkpoint = self.document.summary;
        if let Some(last) = update.window.checkpoint() {
            self.document.summary.cursor = last;
            self.document.summary.raw |= update
                .card
                .as_ref()
                .is_some_and(|card| card.state == State::Raw);
            if !self.document.summary.raw {
                self.document.summary.last_ai = last;
            }
        }
        let result = self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            require_active,
        );
        if result.as_ref().is_err_and(|error| !error.published) {
            self.document.summary = old_checkpoint;
            if inserted {
                self.document.entries.remove(position);
            }
            if let Some(entry) = previous {
                self.document.entries.insert(position, entry);
            }
        } else {
            self.timeline_text_bytes = text_bytes;
            if inserted || index.is_some() {
                self.advance_timeline();
            }
        }
        result
            .map(|()| self.document.summary.cursor)
            .map_err(io::Error::from)
    }
}

fn replacement_entry(update: &Publish, original: Option<&Entry>) -> io::Result<Option<Entry>> {
    let mut replacement = if let Some(card) = &update.card {
        let mut entry = Entry::new("AI_SUMMARY", update.window.started)?;
        entry.fields.title = Zeroizing::new(card.summary.title.clone());
        entry.fields.text = Zeroizing::new(card.summary.text.clone());
        entry.summary = Some(SummaryEntry::from_card(&update.window, card));
        entry
    } else if let Some(original) = original.filter(|entry| {
        !entry.fields.note.trim().is_empty()
            || entry.edited_at.is_some()
            || entry
                .marker
                .as_ref()
                .is_some_and(|marker| marker.source == MarkerSource::User)
    }) {
        let mut entry = Entry::new("USER_NOTE", original.time)?;
        entry.fields.title.clone_from(&original.fields.title);
        entry.fields.text.clone_from(&original.fields.text);
        entry
    } else {
        return Ok(None);
    };
    if let Some(original) = original {
        replacement.id.clone_from(&original.id);
        replacement.fields.note.clone_from(&original.fields.note);
        replacement.marker.clone_from(&original.marker);
        replacement.edited_at = original.edited_at;
    }
    Ok(Some(replacement))
}
