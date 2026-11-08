//! Absolute local-civil time ranges used by the logfile catalog and exports.

use jiff::Span;
use jiff::civil::{Date, DateTime, Time};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Range {
    from: Option<DateTime>,
    to_exclusive: Option<DateTime>,
}

impl Range {
    pub(super) fn is_open(self) -> bool {
        self.from.is_none() && self.to_exclusive.is_none()
    }

    fn reversed(self) -> bool {
        matches!((self.from, self.to_exclusive), (Some(from), Some(to)) if from >= to)
    }

    pub(super) fn overlaps(self, start: DateTime, end: DateTime) -> bool {
        self.to_exclusive.is_none_or(|to| start < to)
            && self.from.is_none_or(|from| start.max(end) >= from)
    }

    pub(super) fn contains(self, timestamp: DateTime) -> bool {
        self.from.is_none_or(|from| timestamp >= from)
            && self.to_exclusive.is_none_or(|to| timestamp < to)
    }

    pub(super) fn tag(self) -> String {
        let tag = |value: Option<DateTime>| {
            value.map_or_else(
                || "open".into(),
                |value| value.strftime("%Y%m%d-%H%M").to_string(),
            )
        };
        format!("{}-{}", tag(self.from), tag(self.to_exclusive))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Field {
    FromDate,
    FromTime,
    ToDate,
    ToTime,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Fields {
    from_date: String,
    from_time: String,
    to_date: String,
    to_time: String,
}

pub(super) struct Evaluation {
    range: Range,
    invalid_from_date: bool,
    invalid_to_date: bool,
    invalid_from_time: bool,
    invalid_to_time: bool,
    reversed: bool,
}

impl Fields {
    pub(super) fn dates(from: Date, to: Date) -> Self {
        Self {
            from_date: from.to_string(),
            to_date: to.to_string(),
            ..Self::default()
        }
    }

    pub(super) fn for_export(preset: Range, start: DateTime, now: DateTime) -> Self {
        let from = preset.from.unwrap_or(start);
        let to = preset
            .to_exclusive
            .and_then(|to| to.checked_sub(Span::new().minutes(1)).ok())
            .unwrap_or(now);
        Self {
            from_date: from.date().to_string(),
            from_time: from.strftime("%H:%M").to_string(),
            to_date: to.date().to_string(),
            to_time: to.strftime("%H:%M").to_string(),
        }
    }

    pub(super) fn set(&mut self, field: Field, value: String) {
        if value.len() > 16 || value.chars().any(char::is_control) {
            return;
        }
        *match field {
            Field::FromDate => &mut self.from_date,
            Field::FromTime => &mut self.from_time,
            Field::ToDate => &mut self.to_date,
            Field::ToTime => &mut self.to_time,
        } = value;
    }

    pub(super) fn text(&self, field: Field) -> &str {
        match field {
            Field::FromDate => &self.from_date,
            Field::FromTime => &self.from_time,
            Field::ToDate => &self.to_date,
            Field::ToTime => &self.to_time,
        }
    }

    pub(super) fn evaluate(&self) -> Evaluation {
        let from_date = parse_date(&self.from_date);
        let to_date = parse_date(&self.to_date);
        let from_time = parse_time(&self.from_time);
        let to_time = parse_time(&self.to_time);
        let from = from_date.map(|date| date.to_datetime(from_time.unwrap_or(Time::MIN)));
        // If the last representable date is selected without a time, no supported
        // timestamp can exceed the end of that day.
        let to_exclusive = to_date.and_then(|date| {
            if let Some(time) = to_time {
                date.to_datetime(time)
                    .checked_add(Span::new().minutes(1))
                    .ok()
            } else {
                date.tomorrow()
                    .ok()
                    .map(|tomorrow| tomorrow.to_datetime(Time::MIN))
            }
        });
        let range = Range { from, to_exclusive };
        Evaluation {
            range,
            invalid_from_date: !self.from_date.trim().is_empty() && from_date.is_none(),
            invalid_to_date: !self.to_date.trim().is_empty() && to_date.is_none(),
            invalid_from_time: !self.from_time.trim().is_empty() && from_time.is_none(),
            invalid_to_time: !self.to_time.trim().is_empty() && to_time.is_none(),
            reversed: range.reversed(),
        }
    }
}

impl Evaluation {
    pub(super) fn catalog(&self) -> Range {
        if self.reversed {
            Range::default()
        } else {
            self.range
        }
    }

    pub(super) fn export(&self) -> Option<Range> {
        self.problem().is_none().then_some(self.range)
    }

    pub(super) fn invalid(&self, field: Field) -> bool {
        self.reversed
            || match field {
                Field::FromDate => self.invalid_from_date,
                Field::FromTime => self.invalid_from_time,
                Field::ToDate => self.invalid_to_date,
                Field::ToTime => self.invalid_to_time,
            }
    }

    pub(super) fn problem(&self) -> Option<&'static str> {
        if self.reversed {
            Some("From must be no later than To.")
        } else if self.invalid_from_date || self.invalid_to_date {
            Some("Use YYYY-MM-DD for the highlighted date.")
        } else if self.invalid_from_time || self.invalid_to_time {
            Some("Use HH:MM for the highlighted time.")
        } else {
            None
        }
    }
}

fn parse_date(text: &str) -> Option<Date> {
    let text = text.trim();
    (text.len() == 10
        && text.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        }))
    .then(|| text.parse().ok())
    .flatten()
}

fn parse_time(text: &str) -> Option<Time> {
    let text = text.trim();
    let number = |value: &str| {
        if (1..=2).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit()) {
            value.parse::<i8>().ok()
        } else {
            None
        }
    };
    let (hour, minute) = if let Some((hours, minutes)) = text.split_once(horizontal_separator) {
        (number(hours)?, number(minutes)?)
    } else if (3..=4).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit()) {
        let (hours, minutes) = text.split_at(text.len() - 2);
        (number(hours)?, number(minutes)?)
    } else {
        (number(text)?, 0)
    };
    Time::new(hour, minute, 0, 0).ok()
}

fn horizontal_separator(character: char) -> bool {
    matches!(
        character,
        ':' | '.' | ' ' | '\t' | '\u{a0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logfile_range_includes_last_millisecond_and_intersecting_older_log() {
        let fields = Fields {
            from_date: "2026-10-30".into(),
            from_time: "8.30".into(),
            to_date: "2026-10-30".into(),
            to_time: "0830".into(),
        };
        let range = fields.evaluate().catalog();
        let timestamp = |value: &str| value.parse::<DateTime>().unwrap();
        let earlier = timestamp("2026-10-30 07:50:00");
        let first = timestamp("2026-10-30 08:30:00");
        let inside = timestamp("2026-10-30 08:30:59.999999999");
        let excluded = timestamp("2026-10-30 08:31:00");
        assert!(!range.contains(earlier));
        assert!(range.contains(first));
        assert!(range.contains(inside));
        assert!(!range.contains(excluded));
        assert!(range.overlaps(earlier, first));
        assert!(!range.overlaps(excluded, excluded));
        assert!(!range.overlaps(earlier, earlier));
        assert_eq!(range.tag(), "20261030-0830-20261030-0831");
    }

    #[test]
    fn logfile_range_handles_open_bounds_bad_times_and_midnight_rollover() {
        let mut fields = Fields {
            to_date: "2026-10-30".into(),
            to_time: "23:59".into(),
            ..Fields::default()
        };
        let midnight = "2026-10-31 00:00:00".parse().unwrap();
        assert!(!fields.evaluate().catalog().contains(midnight));
        fields.set(Field::ToTime, "23:60".into());
        let evaluation = fields.evaluate();
        assert!(evaluation.invalid(Field::ToTime));
        assert!(!evaluation.catalog().contains(midnight));
        assert!(evaluation.export().is_none());
        fields.set(Field::ToTime, String::new());
        fields.set(Field::FromDate, "2026-10-31".into());
        assert!(fields.evaluate().invalid(Field::FromTime));
        assert!(fields.evaluate().catalog().is_open());
        fields.set(Field::FromDate, String::new());
        let evaluation = fields.evaluate();
        assert!(evaluation.export().is_some());
        assert!(!evaluation.catalog().contains(midnight));
    }

    #[test]
    fn logfile_range_time_parser_accepts_the_java_input_forms() {
        for (text, hour, minute) in [
            ("8", 8, 0),
            ("08", 8, 0),
            ("8:0", 8, 0),
            ("8:3", 8, 3),
            ("0800", 8, 0),
            ("800", 8, 0),
            ("8.30", 8, 30),
            ("8 30", 8, 30),
        ] {
            assert_eq!(parse_time(text), Time::new(hour, minute, 0, 0).ok());
        }
        for text in ["", "24:00", "08:60", "08::30", "08  30", "08:30:00"] {
            assert!(parse_time(text).is_none());
        }
    }
}
