use std::num::NonZeroU32;
use std::time::SystemTime;

use kortty_jobs::{
    Date, DependencyMode, Job, JobGraph, JobId, JobSchedule, RunOutcome, RunTrigger, ScheduleError,
    Scheduler, Time, TimeZone, Weekday,
};

fn time(value: &str) -> Time {
    value.parse().unwrap()
}

fn at(value: &str, zone: &TimeZone) -> SystemTime {
    zone.to_timestamp(value.parse().unwrap()).unwrap().into()
}

fn id(value: &str) -> JobId {
    JobId::new(value).unwrap()
}

#[test]
fn uses_fixed_and_interval_times_inside_the_reference_window() {
    // The pinned Java JobScheduleCalculatorTest supplies these first three cases.
    let zone = TimeZone::get("Europe/Berlin").unwrap();
    let fixed = JobSchedule::default()
        .with_weekdays([Weekday::Monday])
        .with_fixed_times([time("10:30")])
        .with_window(time("09:00"), time("12:00"));
    assert_eq!(
        fixed
            .next_run_after(at("2026-05-04T09:00", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-04T10:30", &zone))
    );
    let outside = fixed
        .with_weekdays([Weekday::Monday, Weekday::Tuesday])
        .with_fixed_times([time("18:00")]);
    assert_eq!(
        outside
            .next_run_after(at("2026-05-04T08:00", &zone), &zone)
            .unwrap(),
        None
    );
    let interval = JobSchedule::default()
        .with_interval_minutes(NonZeroU32::new(15))
        .with_window(time("09:00"), time("10:00"));
    assert_eq!(
        interval
            .next_run_after(at("2026-05-04T09:20", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-04T09:30", &zone))
    );
    let combined = interval.with_fixed_times([time("09:25:52"), time("09:25:59")]);
    assert_eq!(
        combined
            .next_run_after(at("2026-05-04T09:20", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-04T09:25", &zone))
    );
    assert_eq!(
        combined
            .next_run_after(at("2026-05-04T09:25", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-04T09:30", &zone))
    );
}

#[test]
fn respects_inclusive_dates_default_trigger_and_bounded_lookahead() {
    let zone = TimeZone::UTC;
    let start: Date = "2026-05-04".parse().unwrap();
    let end: Date = "2026-05-06".parse().unwrap();
    let schedule = JobSchedule::default()
        .with_window(time("11:30"), time("18:00"))
        .with_active_dates(Some(start), Some(end))
        .unwrap();
    assert_eq!(
        schedule
            .next_run_after(at("2026-05-03T14:00", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-04T11:30", &zone))
    );
    assert_eq!(
        schedule
            .next_run_after(at("2026-05-06T11:29", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-06T11:30", &zone))
    );
    assert_eq!(
        schedule
            .next_run_after(at("2026-05-06T11:30", &zone), &zone)
            .unwrap(),
        None
    );
    assert_eq!(
        schedule
            .clone()
            .with_active_dates(Some(end), Some(start))
            .unwrap_err(),
        ScheduleError::ReversedDates
    );
    assert_eq!(
        schedule
            .clone()
            .with_enabled(false)
            .next_run_after(at("2026-05-04T00:00", &zone), &zone)
            .unwrap(),
        None
    );
    let far = schedule
        .with_active_dates(Some("2028-01-01".parse().unwrap()), None)
        .unwrap();
    assert_eq!(
        far.next_run_after(at("2026-05-04T00:00", &zone), &zone)
            .unwrap(),
        None
    );
}

#[test]
fn overnight_intervals_use_the_eligible_anchor_date() {
    let zone = TimeZone::UTC;
    let schedule = JobSchedule::default()
        .with_weekdays([Weekday::Monday])
        .with_window(time("22:00"), time("02:00"))
        .with_interval_minutes(NonZeroU32::new(30));
    assert_eq!(
        schedule
            .next_run_after(at("2026-05-04T23:45", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-05T00:00", &zone))
    );
    // The Java calculator starts its search on the query's local date.
    assert_eq!(
        schedule
            .next_run_after(at("2026-05-05T00:05", &zone), &zone)
            .unwrap(),
        Some(at("2026-05-11T22:00", &zone))
    );
}

#[test]
fn resolves_dst_gaps_forward_and_folds_to_the_first_occurrence() {
    let zone = TimeZone::get("Europe/Berlin").unwrap();
    let schedule = JobSchedule::default().with_fixed_times([time("02:30")]);
    assert_eq!(
        schedule
            .next_run_after(at("2026-03-29T01:00", &zone), &zone)
            .unwrap(),
        Some(at("2026-03-29T03:30", &zone))
    );
    assert_eq!(
        schedule
            .next_run_after(at("2026-10-25T01:00", &zone), &zone)
            .unwrap(),
        Some(at("2026-10-25T00:30", &TimeZone::UTC))
    );
    assert_eq!(
        schedule
            .next_run_after(at("2026-10-25T00:31", &TimeZone::UTC), &zone)
            .unwrap(),
        Some(at("2026-10-26T02:30", &zone))
    );
}

#[test]
fn timer_initialization_skips_past_runs_and_rearms_after_completion() {
    let zone = TimeZone::UTC;
    let calendar = JobSchedule::default()
        .with_interval_minutes(NonZeroU32::new(15))
        .with_window(time("09:00"), time("10:00"));
    let job = Job::new(id("one"), "one", ())
        .unwrap()
        .with_schedule(calendar);
    let mut scheduler = Scheduler::new(JobGraph::new(vec![job]).unwrap());
    assert_eq!(scheduler.next_due(), None);
    scheduler
        .initialize_schedules(zone.clone(), at("2026-05-04T09:15", &zone))
        .unwrap();
    assert_eq!(scheduler.next_due(), Some(at("2026-05-04T09:30", &zone)));
    assert!(
        scheduler
            .tick(at("2026-05-04T09:29", &zone))
            .unwrap()
            .started
            .is_empty()
    );
    let delayed = scheduler.tick(at("2026-05-04T09:47", &zone)).unwrap();
    assert_eq!(delayed.started.len(), 1);
    assert_eq!(delayed.started[0].trigger, RunTrigger::Scheduled);
    assert_eq!(scheduler.next_due(), None);
    assert!(
        scheduler
            .tick(at("2026-05-04T09:50", &zone))
            .unwrap()
            .started
            .is_empty()
    );
    let _ = scheduler
        .complete(
            delayed.started[0].id,
            RunOutcome::Success,
            at("2026-05-04T09:51", &zone),
        )
        .unwrap();
    assert_eq!(
        scheduler.scheduled_at(&id("one")),
        Some(at("2026-05-04T10:00", &zone))
    );
    scheduler.begin_drain();
    assert_eq!(scheduler.next_due(), None);
    assert!(
        scheduler
            .tick(at("2026-05-04T10:00", &zone))
            .unwrap()
            .started
            .is_empty()
    );
}

#[test]
fn blocked_timer_rearms_and_enable_recomputes_from_now() {
    let zone = TimeZone::UTC;
    let root = Job::new(id("root"), "root", ()).unwrap();
    let child = Job::new(id("child"), "child", ())
        .unwrap()
        .with_schedule(JobSchedule::default().with_fixed_times([time("10:00"), time("11:00")]))
        .with_dependencies(DependencyMode::Gate, vec![id("root")], None);
    let mut scheduler = Scheduler::new(JobGraph::new(vec![root, child]).unwrap());
    scheduler
        .initialize_schedules(zone.clone(), at("2026-05-04T09:00", &zone))
        .unwrap();
    let blocked = scheduler.tick(at("2026-05-04T10:00", &zone)).unwrap();
    assert!(blocked.started.is_empty());
    assert_eq!(blocked.finished[0].outcome, RunOutcome::Blocked);
    assert_eq!(scheduler.next_due(), Some(at("2026-05-04T11:00", &zone)));
    assert!(
        scheduler
            .tick(at("2026-05-04T10:30", &zone))
            .unwrap()
            .finished
            .is_empty()
    );
    scheduler
        .set_chain_enabled(&id("root"), false, at("2026-05-04T10:30", &zone))
        .unwrap();
    assert_eq!(scheduler.next_due(), None);
    scheduler
        .set_chain_enabled(&id("root"), true, at("2026-05-04T11:30", &zone))
        .unwrap();
    assert_eq!(scheduler.next_due(), Some(at("2026-05-05T10:00", &zone)));
}
