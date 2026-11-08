use std::collections::HashSet;
use std::io;

use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use regex::{Regex, RegexBuilder};

use super::{Entry, Snapshot};

pub(crate) const DEFAULT_TOLERANCE_MINUTES: u32 = 5;
pub(crate) const MAX_TOLERANCE_MINUTES: u32 = 120;
pub(crate) const MAX_WINDOWS: usize = 32;
pub(crate) const MAX_TOPIC_BYTES: usize = 1024;
const MAX_TOPIC_INPUT_UNITS: usize = 20_000;
const MAX_SELECTED_MARKERS: usize = 4096;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MarkerMode {
    #[default]
    All,
    Marked,
    Selected,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct TimeWindow {
    pub from_date: Option<Date>,
    pub from_time: Option<Time>,
    pub to_date: Option<Date>,
    pub to_time: Option<Time>,
}

impl TimeWindow {
    pub(super) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn is_invalid(&self) -> bool {
        self.from_date
            .zip(self.to_date)
            .is_some_and(|(from, to)| from > to)
    }

    fn overlaps(&self, from: Timestamp, to: Timestamp, zone: &TimeZone) -> bool {
        let from_day = from.to_zoned(zone.clone()).date();
        let to_day = to.to_zoned(zone.clone()).date();
        if self.from_date.is_some_and(|date| to_day < date)
            || self.to_date.is_some_and(|date| from_day > date)
        {
            return false;
        }
        if self.from_time.is_none() && self.to_time.is_none() {
            return true;
        }
        let start = self.from_time.unwrap_or(Time::MIN);
        let end = self.to_time.unwrap_or(Time::MAX);
        let mut day = from_day.yesterday().unwrap_or(from_day);
        while day <= to_day {
            if self.from_date.is_none_or(|date| day >= date)
                && self.to_date.is_none_or(|date| day <= date)
            {
                let end_day = if start > end {
                    day.tomorrow().unwrap_or(day)
                } else {
                    day
                };
                let window_start = day.to_datetime(start).to_zoned(zone.clone());
                let window_end = end_day.to_datetime(end).to_zoned(zone.clone());
                if let (Ok(start), Ok(end)) = (window_start, window_end)
                    && start.timestamp() <= to
                    && end.timestamp() >= from
                {
                    return true;
                }
            }
            let Ok(next) = day.tomorrow() else { break };
            day = next;
        }
        false
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Filter {
    pub windows: Vec<TimeWindow>,
    pub tolerance_minutes: u32,
    pub topic: String,
    pub topic_regex: bool,
    pub marker_mode: MarkerMode,
    pub marker_ids: HashSet<String>,
    pub zone: TimeZone,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            windows: Vec::new(),
            tolerance_minutes: DEFAULT_TOLERANCE_MINUTES,
            topic: String::new(),
            topic_regex: false,
            marker_mode: MarkerMode::All,
            marker_ids: HashSet::new(),
            zone: TimeZone::system(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Preview {
    pub total_entries: usize,
    pub kept_entries: usize,
    pub dropped_by_time: usize,
    pub dropped_by_marker: usize,
    pub dropped_by_topic: usize,
    pub dropped_undated: usize,
}

impl Filter {
    pub(crate) fn is_active(&self) -> bool {
        self.windows.iter().any(|window| !window.is_empty())
            || !self.topic().is_empty()
            || self.marker_mode != MarkerMode::All
    }

    /// Tests a retained capture line as a point in time, with the configured tolerance.
    pub(super) fn matches_capture_time(&self, moment: Timestamp) -> bool {
        if self.windows.iter().all(TimeWindow::is_empty) {
            return true;
        }
        let tolerance = SignedDuration::from_mins(i64::from(self.tolerance_minutes));
        let (Ok(from), Ok(to)) = (
            moment.saturating_sub(tolerance),
            moment.saturating_add(tolerance),
        ) else {
            return false;
        };
        self.windows
            .iter()
            .any(|window| !window.is_empty() && window.overlaps(from, to, &self.zone))
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        self.compile().map(|_| ())
    }

    pub(super) fn topic(&self) -> &str {
        self.topic.trim_matches(|character: char| character <= ' ')
    }

    pub(super) fn compile(&self) -> io::Result<Option<Regex>> {
        if self.windows.len() > MAX_WINDOWS {
            return Err(invalid("Too many journal export time windows"));
        }
        if self.windows.iter().any(TimeWindow::is_invalid) {
            return Err(invalid("The start date must not follow the end date"));
        }
        if self.tolerance_minutes > MAX_TOLERANCE_MINUTES {
            return Err(invalid("Journal export tolerance must be 0–120 minutes"));
        }
        if self.marker_mode == MarkerMode::Selected
            && (self.marker_ids.is_empty() || self.marker_ids.len() > MAX_SELECTED_MARKERS)
        {
            return Err(invalid(
                "Select at least one journal marker within the limit",
            ));
        }
        if self.topic.len() > MAX_TOPIC_BYTES {
            return Err(invalid("Journal export topic exceeds the size limit"));
        }
        let topic = self.topic();
        if topic.is_empty() {
            return Ok(None);
        }
        let literal;
        let pattern = if self.topic_regex {
            topic
        } else {
            literal = regex::escape(topic);
            &literal
        };
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .size_limit(4 * 1024 * 1024)
            .dfa_size_limit(2 * 1024 * 1024)
            .build()
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
    }

    pub(super) fn select<'a>(
        &self,
        snapshot: &'a Snapshot,
    ) -> io::Result<(Preview, Vec<&'a Entry>)> {
        let pattern = self.compile()?;
        let (mut result, mut selected) = self.candidates(snapshot)?;
        if let Some(pattern) = pattern {
            let candidates = selected.len();
            selected.retain(|entry| matches_topic(&pattern, entry));
            result.dropped_by_topic = candidates - selected.len();
        }
        result.kept_entries = selected.len();
        Ok((result, selected))
    }

    pub(super) fn candidates<'a>(
        &self,
        snapshot: &'a Snapshot,
    ) -> io::Result<(Preview, Vec<&'a Entry>)> {
        let time_filter = self.windows.iter().any(|window| !window.is_empty());
        let tolerance = SignedDuration::from_mins(i64::from(self.tolerance_minutes));
        let lookback = tolerance.max(SignedDuration::from_mins(15));
        let mut result = Preview {
            total_entries: snapshot.entries.len(),
            ..Preview::default()
        };
        let mut previous = None;
        let mut selected = Vec::new();
        for entry in &snapshot.entries {
            if time_filter {
                let Some(created) = entry.created_at.map(|date| date.timestamp()) else {
                    result.dropped_undated += 1;
                    continue;
                };
                let earliest = created.saturating_sub(lookback).map_err(io::Error::other)?;
                let span_start =
                    previous.map_or(earliest, |date: Timestamp| date.clamp(earliest, created));
                let from = span_start
                    .saturating_sub(tolerance)
                    .map_err(io::Error::other)?;
                let to = created
                    .saturating_add(tolerance)
                    .map_err(io::Error::other)?;
                previous = Some(created);
                if !self
                    .windows
                    .iter()
                    .filter(|window| !window.is_empty())
                    .any(|window| window.overlaps(from, to, &self.zone))
                {
                    result.dropped_by_time += 1;
                    continue;
                }
            }
            if !self.matches_marker(entry, snapshot) {
                result.dropped_by_marker += 1;
                continue;
            }
            selected.push(entry);
        }
        result.kept_entries = selected.len();
        Ok((result, selected))
    }

    fn matches_marker(&self, entry: &Entry, snapshot: &Snapshot) -> bool {
        match self.marker_mode {
            MarkerMode::All => true,
            MarkerMode::Marked => entry.marker.is_some(),
            MarkerMode::Selected => entry
                .marker
                .is_some_and(|marker| self.marker_ids.contains(&snapshot.markers[marker].id)),
        }
    }
}

pub(super) fn matches_topic(pattern: &Regex, entry: &Entry) -> bool {
    let topic = topic_text(entry);
    !topic.is_empty() && pattern.is_match(&topic)
}

fn topic_text(entry: &Entry) -> String {
    let mut topic = String::new();
    let mut remaining = MAX_TOPIC_INPUT_UNITS;
    for part in [&entry.title, &entry.text, &entry.note]
        .into_iter()
        .chain(&entry.input)
        .chain(&entry.output)
        .filter(|part| !part.is_empty())
    {
        if remaining == 0 {
            break;
        }
        if !topic.is_empty() {
            topic.push('\n');
            remaining -= 1;
        }
        for character in part.chars() {
            let size = character.len_utf16();
            if size > remaining {
                return topic;
            }
            topic.push(character);
            remaining -= size;
        }
    }
    topic
}

/// The Java dialog's accepted shorthand, including compact and separated hours and minutes.
pub(crate) fn parse_time_of_day(raw: &str) -> io::Result<Option<Time>> {
    let raw = raw.trim_matches(|character: char| character <= ' ');
    if raw.is_empty() {
        return Ok(None);
    }
    let (hours, minutes) = if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        match raw.len() {
            1 | 2 => (raw, "0"),
            3 | 4 => raw.split_at(raw.len() - 2),
            _ => return Err(invalid("Invalid journal export time")),
        }
    } else {
        raw.split_once(|c| {
            matches!(
                c,
                ':' | '.' | '\t' | ' ' | '\u{a0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'
                    ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
            )
        })
        .ok_or_else(|| invalid("Invalid journal export time"))?
    };
    let hour = time_part(hours)?;
    let minute = time_part(minutes)?;
    Time::new(hour, minute, 0, 0)
        .map(Some)
        .map_err(|_| invalid("Invalid journal export time"))
}

fn time_part(value: &str) -> io::Result<i8> {
    if !(1..=2).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("Invalid journal export time"));
    }
    value
        .parse()
        .map_err(|_| invalid("Invalid journal export time"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
