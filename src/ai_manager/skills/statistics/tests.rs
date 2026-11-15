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
fn java_skill_history_reports_by_exact_id_without_a_live_catalogue() -> io::Result<()> {
    let profile = Profile::new();
    let root = Directory::open(&profile.0, false)?;
    let source = br#"{"version":1,"tokenizer":"CL100K_BASE","skills":{
        "177":{"name":"Bash","yearly":{"2001":{"automatic":3,"pinned":1,"tokens":140}},
          "snapshots":[{"date":"2001-04-12","tokens":42,"tokenizer":"CL100K_BASE","fingerprint":"01234567deadbeef"}]},
        "another-id":{"name":"Bash","yearly":{"2002":{"automatic":2,"pinned":0,"tokens":60}}},
        "deleted-no-name":{"yearly":{"2001":{"automatic":1,"pinned":1,"tokens":12}}}
    }}"#;
    let import = java::convert(source, None)?;
    assert_eq!(
        (import.historical, import.matched, import.invalid),
        (3, 0, 0)
    );
    let settings = crate::ai_manager::Configuration::default();
    java::publish(
        &root,
        java::decode_draft(&import.bytes)?,
        &settings.skills,
        || true,
    )?;

    let ledger = read(&profile.0)?;
    assert!(ledger.skills.is_empty() && ledger.tokenizer.is_none());
    let current = skill(177, "Bash", 120, 1);
    record(
        &profile.0,
        Surface::Agent,
        &[usage(current.clone(), false, true)],
        || true,
    )?;
    let ledger = read(&profile.0)?;
    assert_eq!(ledger.java_history["177"].yearly["2001"].tokens, 140);
    let mut invalid = ledger.clone();
    invalid.java_history.get_mut("177").unwrap().snapshots = ledger.skills[&177].snapshots.clone();
    let invalid = serde_json::to_vec(&invalid).map_err(io::Error::other)?;
    assert_eq!(
        decode(&invalid).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    let report = ledger.report(
        Range {
            granularity: Granularity::Year,
            periods: 0,
        },
        &[Current {
            skill: current,
            enabled: true,
            learned_rules: 0,
        }],
    );
    assert!(report.has_usage);
    assert_eq!(report.periods.first().map(String::as_str), Some("2001"));
    assert_eq!(
        (
            report.total.automatic,
            report.total.pinned,
            report.total.tokens
        ),
        (7, 2, 332)
    );
    let native = report
        .rows
        .iter()
        .find(|row| row.id == SkillKey::Native(177))
        .unwrap();
    assert_eq!(native.current_tokens, Some(120));
    let historical_key = SkillKey::Java("177".into());
    let historical = report
        .rows
        .iter()
        .find(|row| row.id == historical_key)
        .unwrap();
    assert_eq!(historical.name, native.name);
    assert_eq!(historical.current_tokens, None);
    assert!(!historical.current && historical.hints.is_empty());
    assert_eq!(historical.java_size_history[0].0, Tokenizer::Cl100kBase);
    assert_eq!(historical.java_size_history[0].1[0].tokens, 42);
    assert!(report.rows.iter().any(|row| row.name == "deleted-no-name"));
    let chart = ledger
        .skill_series(
            &historical_key,
            Range {
                granularity: Granularity::Year,
                periods: 0,
            },
        )
        .unwrap();
    assert_eq!(chart.id, Some(historical_key));
    assert_eq!(chart.automatic[0], 3);
    assert_eq!(chart.pinned[0], 1);
    reset(&profile.0)?;
    assert!(read(&profile.0)?.java_history.is_empty());
    Ok(())
}

#[test]
fn java_skill_history_rejects_duplicate_and_excess_ids_and_keeps_old_draft_encoding() {
    let record = SkillRecord {
        name: "Bash".into(),
        ..SkillRecord::default()
    };
    let prior = serde_json::json!({"tokenizer":null,"by_builtin":{"builtin.shell.bash":record}});
    assert!(java::decode_draft(&serde_json::to_vec(&prior).unwrap()).is_ok());
    let duplicate = br#"{"version":1,"skills":{"177":{"name":"Bash"},"177":{"name":"Bash"}}}"#;
    assert!(java::convert(duplicate, None).is_err());
    let record = serde_json::to_string(&record).unwrap();
    let duplicate = format!(
        r#"{{"version":1,"skills":{{}},"java_history":{{"177":{record},"177":{record}}}}}"#
    );
    assert!(decode(duplicate.as_bytes()).is_err());
    let excessive: BTreeMap<_, _> = (0..=MAX_RECORDED_SKILLS)
        .map(|id| (id.to_string(), serde_json::json!({"name":"Bash"})))
        .collect();
    let excessive =
        serde_json::to_vec(&serde_json::json!({"version":1,"skills":excessive})).unwrap();
    assert!(java::convert(&excessive, None).is_err());
}

#[test]
fn java_skill_history_rejects_duplicate_known_fields_and_bucket_keys_per_record() {
    let source = br#"{"version":1,"skills":{
        "duplicate-name":{"name":null,"name":"Duplicate"},
        "duplicate-fingerprint":{"lastFingerprint":null,"lastFingerprint":"ignored"},
        "duplicate-count":{"daily":{"2026-11-03":{"automatic":1,"automatic":2,"pinned":0,"tokens":2}}},
        "duplicate-day":{"daily":{"2026-11-03":{"automatic":1,"pinned":0,"tokens":2},
            "2026-11-03":{"automatic":2,"pinned":0,"tokens":3}}},
        "duplicate-month":{"monthly":{"2026-11":{"automatic":1,"pinned":0,"tokens":2},
            "2026-11":{"automatic":2,"pinned":0,"tokens":3}}},
        "duplicate-year":{"yearly":{"2026":{"automatic":1,"pinned":0,"tokens":2},
            "2026":{"automatic":2,"pinned":0,"tokens":3}}},
        "duplicate-surface":{"daily":{"2026-11-03":{"automatic":1,"pinned":0,"tokens":2,
            "surfaces":{"CHAT":1,"\u0043HAT":2}}}},
        "duplicate-size":{"snapshots":[{"date":"2026-11-03","tokens":2,"tokens":3,
            "tokenizer":"ESTIMATE","fingerprint":"01234567deadbeef"}]},
        "kept":{"name":"Valid skill","lastFingerprint":"01234567deadbeef|ESTIMATE",
            "unknownField":null,"unknownField":{"newFormat":true},
            "daily":{"2026-11-03":{"automatic":1,"pinned":0,"tokens":4,"surfaces":{"CHAT":1}}}}
    }}"#;
    let import = java::convert(source, None).unwrap();
    assert_eq!(
        (import.matched, import.historical, import.invalid),
        (0, 1, 8)
    );
    assert!(java::decode_draft(&import.bytes).is_ok());
    let draft: serde_json::Value = serde_json::from_slice(&import.bytes).unwrap();
    let history = draft["java_history"].as_object().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(
        history["kept"]["daily"]["2026-11-03"]["surfaces"]["chat"],
        1
    );
}

#[test]
fn java_skill_source_replay_keeps_the_first_shape_even_without_readable_settings() -> io::Result<()>
{
    const SETTINGS: &[u8] = br#"<globalSettings><aiSkills>
        <skill><id>source-id</id><builtinId>builtin.shell.bash</builtinId></skill>
    </aiSkills></globalSettings>"#;
    const AMBIGUOUS: &[u8] = br#"<globalSettings><aiSkills>
        <skill><id>source-id</id><builtinId>builtin.shell.bash</builtinId></skill>
        <skill><id>another-id</id><builtinId>builtin.shell.bash</builtinId></skill>
    </aiSkills></globalSettings>"#;
    const MALFORMED: &[u8] = b"<globalSettings";
    const STATISTICS: &[u8] = br#"{"version":1,"skills":{"source-id":{"name":"Bash",
        "yearly":{"2001":{"automatic":3,"pinned":1,"tokens":100}}}}}"#;
    let settings = crate::ai_manager::Configuration::default();
    let native = settings
        .skills
        .skills
        .iter()
        .find_map(|(&id, skill)| {
            (skill.builtin.as_deref() == Some("builtin.shell.bash")).then_some(id)
        })
        .unwrap();
    let bound = java::convert(STATISTICS, Some(SETTINGS))?;
    assert_eq!((bound.matched, bound.historical), (1, 0));
    for (xml, omitted) in [
        (None, false),
        (Some(AMBIGUOUS), false),
        (Some(MALFORMED), true),
    ] {
        let historic = java::convert(STATISTICS, xml)?;
        assert_eq!(
            (
                historic.matched,
                historic.historical,
                historic.settings_omitted
            ),
            (0, 1, omitted)
        );
        for native_first in [true, false] {
            let profile = Profile::new();
            let root = Directory::open(&profile.0, false)?;
            let (first, second) = if native_first {
                (&bound.bytes, &historic.bytes)
            } else {
                (&historic.bytes, &bound.bytes)
            };
            java::publish(&root, java::decode_draft(first)?, &settings.skills, || true)?;
            let original = std::fs::read(profile.0.join(BACKUP_PATH))?;
            java::publish(&root, java::decode_draft(second)?, &settings.skills, || {
                true
            })?;
            assert_eq!(std::fs::read(profile.0.join(BACKUP_PATH))?, original);
            let ledger = read(&profile.0)?;
            if native_first {
                assert_eq!(
                    ledger.java_bindings["source-id"],
                    java::Disposition::Bound(native)
                );
                assert!(ledger.java_history.is_empty());
            } else {
                assert!(ledger.java_bindings.is_empty() && ledger.skills.is_empty());
                assert!(ledger.java_history.contains_key("source-id"));
            }
            assert_eq!(
                ledger
                    .report(
                        Range {
                            granularity: Granularity::Year,
                            periods: 0
                        },
                        &[]
                    )
                    .total
                    .tokens,
                100
            );
        }

        let profile = Profile::new();
        let root = Directory::open(&profile.0, false)?;
        record(
            &profile.0,
            Surface::Agent,
            &[usage(skill(native, "Native Bash", 29, 1), false, true)],
            || true,
        )?;
        java::publish(
            &root,
            java::decode_draft(&bound.bytes)?,
            &settings.skills,
            || true,
        )?;
        let reserved = std::fs::read(profile.0.join(BACKUP_PATH))?;
        java::publish(
            &root,
            java::decode_draft(&historic.bytes)?,
            &settings.skills,
            || true,
        )?;
        assert_eq!(std::fs::read(profile.0.join(BACKUP_PATH))?, reserved);
        let ledger = read(&profile.0)?;
        assert_eq!(
            ledger.java_bindings["source-id"],
            java::Disposition::Reserved
        );
        assert!(ledger.java_history.is_empty());
        assert_eq!(ledger.report(Range::default(), &[]).total.tokens, 29);
    }
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
