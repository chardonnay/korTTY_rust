use std::cell::Cell;
use std::io;
use std::thread;

use crate::snippets::tests::Profile;

use super::*;

fn skill(id: u64, name: &str, tokens: u64, revision: u8) -> Observation {
    Observation {
        id,
        name: name.to_owned(),
        tokens,
        tokenizer: Tokenizer::Cl100kBase,
        fingerprint: [revision; 32],
    }
}

fn usage(skill: Observation, pinned: bool, full_size: bool) -> Usage {
    Usage {
        skill,
        pinned,
        full_size,
    }
}

#[test]
fn skill_statistics_persist_only_committed_sizes_and_deduplicate_one_request() -> io::Result<()> {
    let profile = Profile::new();
    let initial = skill(1, "Python", 120, 1);
    let partial = skill(1, "Python", 20, 2);
    let complete = skill(1, "Python", 180, 2);
    let allowed = || true;
    assert!(read(&profile.0)?.skills.is_empty());
    assert!(!profile.0.join(FOLDER).exists());
    record(
        &profile.0,
        Surface::Chat,
        &[usage(initial.clone(), false, true)],
        || false,
    )?;
    assert!(!profile.0.join(FOLDER).exists());

    record(
        &profile.0,
        Surface::Chat,
        &[
            usage(initial.clone(), false, true),
            usage(initial, true, true),
        ],
        allowed,
    )?;
    record(
        &profile.0,
        Surface::Agent,
        &[usage(partial, true, false)],
        allowed,
    )?;
    let reopened = read(&profile.0)?;
    assert_eq!(reopened.tokenizer(), Tokenizer::Cl100kBase);
    assert_eq!(reopened.skills[&1].snapshots.len(), 1);
    assert_eq!(reopened.skills[&1].snapshots[0].tokens, 120);
    let report = reopened.report(Range::default(), &[]);
    assert!(report.has_usage);
    assert_eq!(
        (
            report.total.automatic,
            report.total.pinned,
            report.total.tokens
        ),
        (1, 1, 140)
    );
    assert_eq!(
        report.total.surfaces,
        BTreeMap::from([(Surface::Chat, 1), (Surface::Agent, 1)])
    );
    assert_eq!(report.rows[0].average_tokens, Some(70.0));

    observe(&profile.0, std::slice::from_ref(&complete), allowed)?;
    observe(&profile.0, std::slice::from_ref(&complete), allowed)?;
    let reopened = read(&profile.0)?;
    assert_eq!(reopened.skills[&1].snapshots.len(), 2);
    let report = reopened.report(
        Range::default(),
        &[Current {
            skill: complete,
            enabled: true,
            learned_rules: 1,
        }],
    );
    assert_eq!(report.average_size_tokens, Some(180.0));
    assert_eq!(report.rows[0].history.len(), 1);
    assert_eq!(report.rows[0].history[0].tokens, 180);
    assert_eq!(report.rows[0].learned_rules, 1);
    assert_eq!(report.total.uses(), 2);
    reset(&profile.0)?;
    assert!(read(&profile.0)?.skills.is_empty());
    Ok(())
}

#[test]
fn skill_statistics_recheck_permission_at_publication_and_keep_malformed_data() -> io::Result<()> {
    let profile = Profile::new();
    let events = [usage(skill(1, "Python", 30, 1), false, true)];
    let checks = Cell::new(0);
    let error = record(&profile.0, Surface::Workflow, &events, || {
        checks.set(checks.get() + 1);
        checks.get() < 3
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(read(&profile.0)?.skills.is_empty());
    assert!(!profile.0.join(FOLDER).join(FILE).exists());

    let directory = open_directory(&profile.0, true)?.unwrap();
    directory.publish_copy(FILE, b"unreadable existing record")?;
    assert_eq!(
        read(&profile.0).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        record(&profile.0, Surface::Workflow, &events, || true)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData,
    );
    let mut contents = Vec::new();
    std::io::Read::read_to_end(&mut directory.open_private_file(FILE)?, &mut contents)?;
    assert_eq!(contents, b"unreadable existing record");
    reset(&profile.0)?;
    record(&profile.0, Surface::Workflow, &events, || true)?;
    let checks = Cell::new(0);
    assert_eq!(
        record(&profile.0, Surface::Workflow, &events, || {
            checks.set(checks.get() + 1);
            checks.get() < 3
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::PermissionDenied,
    );
    assert_eq!(
        read(&profile.0)?.report(Range::default(), &[]).total.uses(),
        1
    );
    Ok(())
}

#[test]
fn skill_statistics_retention_folds_only_old_periods_and_keeps_last_iso_week_size() -> io::Result<()>
{
    let today = Date::constant(2026, 11, 12);
    let daily_cutoff = today.checked_sub(Span::new().days(400)).unwrap();
    let monthly_cutoff = today
        .first_of_month()
        .checked_sub(Span::new().months(60))
        .unwrap();
    let mut ledger = Ledger::default();
    for (date, id, tokens, revision, pinned) in [
        (monthly_cutoff, 1, 60, 1, false),
        (daily_cutoff, 1, 90, 2, true),
        (Date::constant(2025, 12, 29), 1, 120, 3, false),
        (Date::constant(2026, 1, 2), 1, 180, 4, true),
        (today, 1, 240, 5, false),
    ] {
        ledger.record_at(
            Surface::Agent,
            &[usage(skill(id, "Python", tokens, revision), pinned, true)],
            date,
        )?;
    }
    ledger.roll_up(today);
    let first = ledger.clone();
    ledger.roll_up(today);
    assert_eq!(ledger, first);
    let record = &ledger.skills[&1];
    assert!(record.yearly.contains_key("2021"));
    assert!(!record.monthly.contains_key(&month_key(monthly_cutoff)));
    assert!(record.monthly.contains_key(&month_key(daily_cutoff)));
    assert!(!record.daily.contains_key(&daily_cutoff.to_string()));
    assert!(record.daily.contains_key(&today.to_string()));
    assert!(
        !record
            .snapshots
            .iter()
            .any(|point| point.date == "2025-12-29")
    );
    assert!(
        record
            .snapshots
            .iter()
            .any(|point| point.date == "2026-01-02")
    );

    let today_report = ledger.report_at(
        Range {
            granularity: Granularity::Day,
            periods: 1,
        },
        &[],
        today,
    );
    assert_eq!(
        (today_report.total.uses(), today_report.total.tokens),
        (1, 240)
    );
    let year_report = ledger.report_at(
        Range {
            granularity: Granularity::Year,
            periods: 0,
        },
        &[],
        today,
    );
    assert_eq!(
        year_report.periods,
        ["2021", "2022", "2023", "2024", "2025", "2026"]
    );
    assert_eq!(
        (
            year_report.total.automatic,
            year_report.total.pinned,
            year_report.total.tokens
        ),
        (3, 2, 690)
    );
    assert_eq!(year_report.series[0].automatic, [1, 0, 0, 0, 1, 1]);
    assert_eq!(year_report.series[0].pinned, [0, 0, 0, 0, 1, 1]);
    Ok(())
}

#[test]
fn skill_statistics_shared_profile_serializes_parallel_updates() -> io::Result<()> {
    let profile = Profile::new();
    let events = [usage(skill(1, "Python", 12, 1), false, true)];
    thread::scope(|scope| -> io::Result<()> {
        let a = scope.spawn(|| record(&profile.0, Surface::Chat, &events, || true));
        let b = scope.spawn(|| record(&profile.0, Surface::Agent, &events, || true));
        a.join().unwrap()?;
        b.join().unwrap()
    })?;
    let report = read(&profile.0)?.report(Range::default(), &[]);
    assert_eq!(report.total.automatic, 2);
    assert_eq!(report.total.tokens, 24);
    assert_eq!(
        report.total.surfaces,
        BTreeMap::from([(Surface::Chat, 1), (Surface::Agent, 1)])
    );
    assert_eq!(report.rows[0].history.len(), 1);
    Ok(())
}
