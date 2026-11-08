//! Local civil-time intervals for automatic profile backups.

use jiff::tz::OffsetConflict;
use jiff::{Span, Timestamp, Zoned};

use super::preferences::Interval;

/// The first enabled run is due immediately. Later runs retain the local time
/// of the last successful backup, including the prior offset when a fold allows it.
pub(crate) fn due(
    interval: Interval,
    last: Option<Timestamp>,
    now: &Zoned,
) -> Result<bool, jiff::Error> {
    let span = match interval {
        Interval::Off => return Ok(false),
        Interval::Daily => Span::new().days(1),
        Interval::Weekly => Span::new().weeks(1),
        Interval::Monthly => Span::new().months(1),
    };
    let Some(last) = last else {
        return Ok(true);
    };
    let zone = now.time_zone().clone();
    let previous = last.to_zoned(zone.clone());
    let next_date = previous.datetime().checked_add(span)?;
    let next = OffsetConflict::PreferOffset
        .resolve(next_date, previous.offset(), zone)?
        .compatible()?;
    Ok(now.timestamp() >= next.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    fn timestamp(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    fn assert_boundary(interval: Interval, last: &str, next: &str, zone: &TimeZone) {
        let last = Some(timestamp(last));
        let next = timestamp(next);
        let before = Timestamp::from_second(next.as_second() - 1).unwrap();
        assert!(!due(interval, last, &before.to_zoned(zone.clone())).unwrap());
        assert!(due(interval, last, &next.to_zoned(zone.clone())).unwrap());
    }

    #[test]
    fn first_run_and_disabled_schedule_have_distinct_due_semantics() {
        let now = timestamp("2026-01-31T08:00:00Z").to_zoned(TimeZone::UTC);
        assert!(!due(Interval::Off, None, &now).unwrap());
        assert!(!due(Interval::Off, Some(now.timestamp()), &now).unwrap());
        for interval in [Interval::Daily, Interval::Weekly, Interval::Monthly] {
            assert!(due(interval, None, &now).unwrap());
            assert!(!due(interval, Some(now.timestamp()), &now).unwrap());
        }
    }

    #[test]
    fn civil_intervals_match_java_boundaries_and_clamp_shorter_months() {
        let last = "2026-01-31T08:00:00Z";
        for (interval, next) in [
            (Interval::Daily, "2026-02-01T08:00:00Z"),
            (Interval::Weekly, "2026-02-07T08:00:00Z"),
            (Interval::Monthly, "2026-02-28T08:00:00Z"),
        ] {
            assert_boundary(interval, last, next, &TimeZone::UTC);
        }
        assert_boundary(
            Interval::Monthly,
            "2028-01-31T08:00:00Z",
            "2028-02-29T08:00:00Z",
            &TimeZone::UTC,
        );
    }

    #[test]
    fn spring_gap_moves_forward_and_weekly_keeps_local_clock_time() {
        let zone = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_boundary(
            Interval::Daily,
            "2026-03-07T07:30:00Z",
            "2026-03-08T07:30:00Z",
            &zone,
        );
        assert_boundary(
            Interval::Weekly,
            "2026-03-06T17:00:00Z",
            "2026-03-13T16:00:00Z",
            &zone,
        );
    }

    #[test]
    fn autumn_fold_preserves_the_previous_offset_when_possible() {
        // The first date is on standard time. A short DST season between the
        // two dates makes the second occurrence the one with that same offset.
        let zone = TimeZone::posix("EST5EDT,M10.5.0,M11.1.0").unwrap();
        assert_boundary(
            Interval::Monthly,
            "2026-10-01T06:30:00Z",
            "2026-11-01T06:30:00Z",
            &zone,
        );
    }
}
