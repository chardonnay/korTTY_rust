use std::ops::RangeInclusive;

use iced::Element;
use iced::widget::{button, row, text};
use jiff::Timestamp;

use crate::session_journal::Journal;
use crate::session_journal::export::MAX_WINDOWS;

use super::{Format, JournalMessage, Message};

#[derive(Clone, Debug)]
pub enum Event {
    Select(usize, String),
    Add,
    Apply,
    Cancel,
}

pub(in crate::workspace::journal_access) struct Picker {
    pub(super) ticket: u64,
    pub(super) format: Format,
    pub(in crate::workspace::journal_access) page: usize,
    start: Option<usize>,
    end: Option<usize>,
    windows: Vec<(Timestamp, Timestamp)>,
}

impl Picker {
    pub(super) fn new(ticket: u64, format: Format) -> Self {
        Self {
            ticket,
            format,
            page: 0,
            start: None,
            end: None,
            windows: Vec::new(),
        }
    }

    pub(super) fn select(&mut self, document: &Journal, index: usize, id: &str) {
        if self.windows.len() == MAX_WINDOWS
            || !document
                .timeline
                .get(index)
                .is_some_and(|entry| entry.id == id && entry.created_at.is_some())
        {
            return;
        }
        if self.start.is_some() && self.end.is_none() {
            self.end = Some(index);
        } else {
            self.start = Some(index);
            self.end = None;
        }
    }

    fn current(&self) -> Option<RangeInclusive<usize>> {
        let start = self.start?;
        let end = self.end.unwrap_or(start);
        Some(start.min(end)..=start.max(end))
    }

    fn timestamps(&self, document: &Journal) -> Option<(Timestamp, Timestamp)> {
        let range = self.current()?;
        let start = document.timeline.get(*range.start())?.created_at?;
        let end = document.timeline.get(*range.end())?.created_at?;
        Some((start.min(end), start.max(end)))
    }

    pub(super) fn add(&mut self, document: &Journal) {
        if self.windows.len() < MAX_WINDOWS
            && let Some(window) = self.timestamps(document)
        {
            self.windows.push(window);
            self.start = None;
            self.end = None;
        }
    }

    pub(super) fn apply(&mut self, document: &Journal) -> Vec<(Timestamp, Timestamp)> {
        self.add(document);
        std::mem::take(&mut self.windows)
    }

    pub(in crate::workspace::journal_access) fn highlights(&self, index: usize) -> bool {
        self.current().is_some_and(|range| range.contains(&index))
    }

    pub(in crate::workspace::journal_access) fn event(&self, event: Event) -> JournalMessage {
        JournalMessage::Export(Message::Range(self.ticket, event))
    }

    pub(in crate::workspace::journal_access) fn view(
        &self,
        document: &Journal,
    ) -> Element<'_, JournalMessage> {
        let current = self.timestamps(document);
        let prompt = current.map_or_else(
            || "Click the first and last entry".into(),
            |(start, end)| {
                format!(
                    "{} – {}",
                    start
                        .to_zoned(jiff::tz::TimeZone::system())
                        .strftime("%F %R"),
                    end.to_zoned(jiff::tz::TimeZone::system()).strftime("%F %R")
                )
            },
        );
        row![
            text(prompt).size(13),
            text(format!("{} saved range(s)", self.windows.len())).size(13),
            button("Add range").on_press_maybe(current.is_some().then(|| self.event(Event::Add))),
            button("Use for export").on_press_maybe(
                (current.is_some() || !self.windows.is_empty()).then(|| self.event(Event::Apply))
            ),
            button("Cancel").on_press(self.event(Event::Cancel)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .wrap()
        .into()
    }
}
