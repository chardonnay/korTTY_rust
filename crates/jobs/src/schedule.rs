use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::time::SystemTime;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{Date, Time, TimeZone, Weekday};

/// Number of calendar dates searched, including the local date of the query.
pub const SCHEDULE_SEARCH_DAYS: usize = 370;
const DAY_MINUTES: u16 = 24 * 60;

/// Minute-resolution schedule with inclusive dates and a daily time window.
/// Fixed times and window-anchored intervals may be used together.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "ScheduleFields")]
pub struct JobSchedule {
    enabled: bool,
    from: Option<Date>,
    until: Option<Date>,
    weekdays: u8,
    fixed: BTreeSet<u16>,
    window_start: u16,
    window_end: u16,
    interval_minutes: Option<NonZeroU32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleFields {
    enabled: bool,
    from: Option<Date>,
    until: Option<Date>,
    weekdays: u8,
    fixed: BTreeSet<u16>,
    window_start: u16,
    window_end: u16,
    interval_minutes: Option<NonZeroU32>,
}

impl TryFrom<ScheduleFields> for JobSchedule {
    type Error = ScheduleError;

    fn try_from(fields: ScheduleFields) -> Result<Self, Self::Error> {
        if fields.weekdays & !0x7f != 0
            || fields.window_start >= DAY_MINUTES
            || fields.window_end >= DAY_MINUTES
            || fields
                .fixed
                .last()
                .is_some_and(|&minute| minute >= DAY_MINUTES)
        {
            return Err(ScheduleError::InvalidCalendar);
        }
        Self {
            enabled: fields.enabled,
            from: None,
            until: None,
            weekdays: fields.weekdays,
            fixed: fields.fixed,
            window_start: fields.window_start,
            window_end: fields.window_end,
            interval_minutes: fields.interval_minutes,
        }
        .with_active_dates(fields.from, fields.until)
    }
}

impl Default for JobSchedule {
    fn default() -> Self {
        Self {
            enabled: true,
            from: None,
            until: None,
            weekdays: 0,
            fixed: BTreeSet::new(),
            window_start: 0,
            window_end: DAY_MINUTES - 1,
            interval_minutes: None,
        }
    }
}

impl JobSchedule {
    /// Enables or disables automatic attempts for this schedule.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Sets an inclusive date range. Either endpoint may be omitted.
    pub fn with_active_dates(
        mut self,
        from: Option<Date>,
        until: Option<Date>,
    ) -> Result<Self, ScheduleError> {
        if matches!((from, until), (Some(from), Some(until)) if from > until) {
            return Err(ScheduleError::ReversedDates);
        }
        self.from = from;
        self.until = until;
        Ok(self)
    }

    /// Sets the allowed weekdays. An empty set permits every day.
    #[must_use]
    pub fn with_weekdays(mut self, weekdays: impl IntoIterator<Item = Weekday>) -> Self {
        self.weekdays = weekdays
            .into_iter()
            .fold(0, |set, weekday| set | weekday_bit(weekday));
        self
    }

    /// Sets fixed times, rounded down to whole minutes and deduplicated.
    #[must_use]
    pub fn with_fixed_times(mut self, times: impl IntoIterator<Item = Time>) -> Self {
        self.fixed = times.into_iter().map(minute).collect();
        self
    }

    /// Sets the inclusive daily time window. An earlier end crosses midnight.
    #[must_use]
    pub fn with_window(mut self, start: Time, end: Time) -> Self {
        self.window_start = minute(start);
        self.window_end = minute(end);
        self
    }

    /// Sets an optional interval anchored at the start of each allowed window.
    #[must_use]
    pub fn with_interval_minutes(mut self, interval: Option<NonZeroU32>) -> Self {
        self.interval_minutes = interval;
        self
    }

    /// Whether the schedule is enabled independently of its job.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Inclusive start and end dates, each optional.
    pub fn active_dates(&self) -> (Option<Date>, Option<Date>) {
        (self.from, self.until)
    }

    /// Explicit weekday filter in Monday-first order. An empty filter permits every day.
    pub fn weekdays(&self) -> impl Iterator<Item = Weekday> + '_ {
        [
            Weekday::Monday,
            Weekday::Tuesday,
            Weekday::Wednesday,
            Weekday::Thursday,
            Weekday::Friday,
            Weekday::Saturday,
            Weekday::Sunday,
        ]
        .into_iter()
        .filter(|&day| self.weekdays & weekday_bit(day) != 0)
    }

    /// Fixed times in ascending order at minute resolution.
    pub fn fixed_times(&self) -> impl Iterator<Item = Time> + '_ {
        self.fixed.iter().copied().map(time_at_minute)
    }

    /// Inclusive daily start and end. A descending window crosses midnight.
    pub fn window(&self) -> (Time, Time) {
        (
            time_at_minute(self.window_start),
            time_at_minute(self.window_end),
        )
    }

    /// Optional recurring interval within the daily window.
    pub fn interval_minutes(&self) -> Option<NonZeroU32> {
        self.interval_minutes
    }

    /// Returns the first execution strictly after the supplied instant in the
    /// supplied time zone. A DST gap moves forward by its duration and a fold
    /// uses the earlier offset, matching the reference calendar.
    ///
    /// With no fixed times or interval, the window start is the daily trigger.
    /// The search begins on the query's local date. An overnight interval can
    /// yield an occurrence on the following day from an eligible anchor date.
    pub fn next_run_after(
        &self,
        after: SystemTime,
        zone: &TimeZone,
    ) -> Result<Option<SystemTime>, ScheduleError> {
        if !self.enabled {
            return Ok(None);
        }
        let after = timestamp(after)?;
        Ok(self.next_timestamp_after(after, zone).map(SystemTime::from))
    }

    pub(crate) fn next_timestamp_after(
        &self,
        after: Timestamp,
        zone: &TimeZone,
    ) -> Option<Timestamp> {
        if !self.enabled {
            return None;
        }
        let mut day = after.to_zoned(zone.clone()).date();
        let overnight = self.window_end < self.window_start;
        let fixed: Vec<_> = self
            .fixed
            .iter()
            .copied()
            .filter(|&time| {
                if overnight {
                    time >= self.window_start || time <= self.window_end
                } else {
                    (self.window_start..=self.window_end).contains(&time)
                }
            })
            .collect();
        if !self.fixed.is_empty() && fixed.is_empty() && self.interval_minutes.is_none() {
            return None;
        }
        for _ in 0..SCHEDULE_SEARCH_DAYS {
            if self.until.is_some_and(|until| day > until) {
                break;
            }
            let tomorrow = day.tomorrow().ok();
            if self.from.is_none_or(|from| day >= from)
                && (self.weekdays == 0 || self.weekdays & weekday_bit(day.weekday()) != 0)
            {
                let mut next = None;
                let mut consider = |date: Date, minute: u16| {
                    let time = date.at((minute / 60) as i8, (minute % 60) as i8, 0, 0);
                    if let Ok(candidate) = zone.to_timestamp(time)
                        && candidate > after
                        && next.is_none_or(|next| candidate < next)
                    {
                        next = Some(candidate);
                    }
                };
                for &time in &fixed {
                    consider(day, time);
                }
                if let Some(interval) = self.interval_minutes {
                    let end = self.window_end + if overnight { DAY_MINUTES } else { 0 };
                    for cursor in (self.window_start..=end).step_by(interval.get() as usize) {
                        if cursor < DAY_MINUTES {
                            consider(day, cursor);
                        } else if let Some(tomorrow) = tomorrow {
                            consider(tomorrow, cursor - DAY_MINUTES);
                        }
                    }
                } else if self.fixed.is_empty() {
                    consider(day, self.window_start);
                }
                if next.is_some() {
                    return next;
                }
            }
            day = tomorrow?;
        }
        None
    }
}

pub(crate) fn timestamp(time: SystemTime) -> Result<Timestamp, ScheduleError> {
    Timestamp::try_from(time).map_err(|_| ScheduleError::TimeOutOfRange)
}

fn weekday_bit(day: Weekday) -> u8 {
    1 << day.to_monday_zero_offset()
}

fn minute(time: Time) -> u16 {
    60 * time.hour() as u16 + time.minute() as u16
}

fn time_at_minute(minute: u16) -> Time {
    Time::midnight().saturating_add(jiff::Span::new().minutes(minute))
}

/// Invalid calendar configuration or an instant outside the calendar range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScheduleError {
    ReversedDates,
    TimeOutOfRange,
    InvalidCalendar,
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReversedDates => "schedule start date is after its end date",
            Self::TimeOutOfRange => "instant is outside the calendar range",
            Self::InvalidCalendar => "calendar contains an invalid time or weekday mask",
        })
    }
}

impl Error for ScheduleError {}
