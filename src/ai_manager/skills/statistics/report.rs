//! Range, history and usage aggregates for the skill statistics dialog.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use jiff::{Span, Zoned, civil::Date};

use super::{Bucket, Ledger, Observation, SkillRecord, day, display_name, month, month_key, year};

const MIN_DATE: Date = Date::constant(1, 1, 1);
const TOP_SERIES: usize = 8;
const DAYS_PER_MONTH: f64 = 30.44;

#[derive(Clone, Debug)]
pub(crate) struct Current {
    pub(crate) skill: Observation,
    pub(crate) enabled: bool,
    pub(crate) learned_rules: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Granularity {
    #[default]
    Day,
    Month,
    Year,
}

impl fmt::Display for Granularity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Day => "Day",
            Self::Month => "Month",
            Self::Year => "Year",
        })
    }
}

impl Granularity {
    fn start(self, date: Date) -> Date {
        match self {
            Self::Day => date,
            Self::Month => date.first_of_month(),
            Self::Year => date.first_of_year(),
        }
    }

    fn span(self, count: i64) -> Span {
        match self {
            Self::Day => Span::new().days(count),
            Self::Month => Span::new().months(count),
            Self::Year => Span::new().years(count),
        }
    }

    fn label(self, date: Date) -> String {
        match self {
            Self::Day => date.to_string(),
            Self::Month => month_key(date),
            Self::Year => format!("{:04}", date.year()),
        }
    }

    fn date(self, key: &str) -> Option<Date> {
        match self {
            Self::Day => day(key),
            Self::Month => month(key),
            Self::Year => year(key),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Range {
    pub(crate) granularity: Granularity,
    /// Zero selects the usual day or month range, or all recorded years.
    pub(crate) periods: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Report {
    pub(crate) periods: Vec<String>,
    pub(crate) total: Bucket,
    pub(crate) rows: Vec<Row>,
    pub(crate) series: Vec<Series>,
    pub(crate) average_size_tokens: Option<f64>,
    pub(crate) growth_tokens_per_month: Option<f64>,
    pub(crate) has_usage: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) current: bool,
    pub(crate) enabled: bool,
    pub(crate) usage: Bucket,
    pub(crate) average_tokens: Option<f64>,
    pub(crate) current_tokens: Option<u64>,
    pub(crate) growth_tokens_per_month: Option<f64>,
    pub(crate) growth_percent: Option<f64>,
    pub(crate) last_used: Option<String>,
    pub(crate) learned_rules: usize,
    pub(crate) history: Vec<Point>,
    pub(crate) hints: Vec<Hint>,
}

#[derive(Clone, Debug)]
pub(crate) struct Series {
    pub(crate) id: Option<u64>,
    pub(crate) name: String,
    pub(crate) automatic: Vec<u64>,
    pub(crate) pinned: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Point {
    pub(crate) date: String,
    pub(crate) tokens: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Hint {
    Unused,
    Growing,
    Large,
}

impl fmt::Display for Hint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unused => "Unused for at least 90 days",
            Self::Growing => "More than doubled in size",
            Self::Large => "Over 1,000 tokens",
        })
    }
}

impl Ledger {
    pub(crate) fn report(&self, range: Range, current: &[Current]) -> Report {
        self.report_at(range, current, Zoned::now().date())
    }

    pub(crate) fn skill_series(&self, id: u64, range: Range) -> Option<Series> {
        let record = self.skills.get(&id)?;
        let today = Zoned::now().date();
        let dates = periods(self, range, today);
        let (first, last) = (*dates.first()?, *dates.last()?);
        let buckets = period_buckets(record, range.granularity, first, last);
        Some(series_of(Some(id), record.name.clone(), &buckets, &dates))
    }

    pub(super) fn report_at(&self, range: Range, current: &[Current], today: Date) -> Report {
        let unit = range.granularity;
        let periods = periods(self, range, today);
        let from = *periods.first().unwrap_or(&today);
        let to = *periods.last().unwrap_or(&today);
        let current: BTreeMap<_, _> = current
            .iter()
            .map(|skill| (skill.skill.id, skill))
            .collect();
        let ids: BTreeSet<_> = self.skills.keys().chain(current.keys()).copied().collect();
        let mut entries = Vec::with_capacity(ids.len());
        let mut total = Bucket::default();
        for id in ids {
            let record = self.skills.get(&id);
            let buckets = record.map_or_else(BTreeMap::new, |record| {
                period_buckets(record, unit, from, to)
            });
            let mut usage = Bucket::default();
            for bucket in buckets.values() {
                usage.add(bucket);
            }
            total.add(&usage);
            let live = current.get(&id).copied();
            let history = record.map_or_else(BTreeMap::new, |record| {
                record
                    .snapshots
                    .iter()
                    .filter(|snapshot| snapshot.identity.tokenizer == self.tokenizer())
                    .filter_map(|snapshot| day(&snapshot.date).map(|day| (day, snapshot.tokens)))
                    .collect()
            });
            let current_tokens = live.map(|skill| skill.skill.tokens);
            let (growth_tokens_per_month, growth_percent) =
                current_tokens.map_or((None, None), |tokens| growth(&history, tokens, from, today));
            let last_used = record.and_then(|record| record.last_used.clone());
            let hints = current_tokens.map_or_else(Vec::new, |tokens| {
                hints(&history, last_used.as_deref().and_then(day), tokens, today)
            });
            let uses = usage.uses();
            let row = Row {
                id,
                name: live
                    .map(|skill| display_name(&skill.skill.name))
                    .or_else(|| record.map(|record| record.name.clone()))
                    .unwrap_or_default(),
                current: live.is_some(),
                enabled: live.is_some_and(|skill| skill.enabled),
                average_tokens: (uses != 0).then(|| usage.tokens as f64 / uses as f64),
                usage,
                current_tokens,
                growth_tokens_per_month,
                growth_percent,
                last_used,
                learned_rules: live.map_or(0, |skill| skill.learned_rules),
                history: history
                    .into_iter()
                    .map(|(date, tokens)| Point {
                        date: date.to_string(),
                        tokens,
                    })
                    .collect(),
                hints,
            };
            entries.push((row, buckets));
        }
        let series = series(&entries, &periods);
        let mut rows: Vec<_> = entries.into_iter().map(|(row, _)| row).collect();
        rows.sort_by_cached_key(|row| {
            (
                Reverse(row.usage.automatic),
                row.name.to_lowercase(),
                row.id,
            )
        });
        let (mut size_sum, mut size_count, mut growth_sum, mut growth_count) = (0.0, 0, 0.0, 0);
        for row in rows.iter().filter(|row| row.enabled) {
            if let Some(tokens) = row.current_tokens {
                size_sum += tokens as f64;
                size_count += 1;
            }
            if let Some(growth) = row.growth_tokens_per_month {
                growth_sum += growth;
                growth_count += 1;
            }
        }
        let has_usage = self.skills.values().any(|record| {
            record
                .daily
                .values()
                .chain(record.monthly.values())
                .chain(record.yearly.values())
                .any(|bucket| bucket.uses() != 0)
        });
        Report {
            periods: periods.into_iter().map(|date| unit.label(date)).collect(),
            total,
            rows,
            series,
            average_size_tokens: (size_count != 0).then(|| size_sum / f64::from(size_count)),
            growth_tokens_per_month: (growth_count != 0).then_some(growth_sum),
            has_usage,
        }
    }
}

fn periods(ledger: &Ledger, range: Range, today: Date) -> Vec<Date> {
    let unit = range.granularity;
    let today = unit.start(today.max(MIN_DATE));
    let count = match (unit, range.periods) {
        (Granularity::Day, 0) => 30,
        (Granularity::Month, 0) => 12,
        (Granularity::Year, 0) => {
            let earliest = ledger
                .skills
                .values()
                .flat_map(|record| {
                    record
                        .daily
                        .keys()
                        .next()
                        .into_iter()
                        .chain(record.monthly.keys().next())
                        .chain(record.yearly.keys().next())
                })
                .filter_map(|key| key.get(..4)?.parse::<i16>().ok())
                .fold(today.year(), i16::min)
                .max(MIN_DATE.year());
            (today.year() - earliest + 1) as usize
        }
        (Granularity::Day, count) => count.min(400),
        (Granularity::Month, count) => count.min(60),
        (Granularity::Year, count) => count.min(today.year() as usize),
    };
    (0..count)
        .rev()
        .filter_map(|back| today.checked_sub(unit.span(back as i64)).ok())
        .filter(|date| *date >= MIN_DATE)
        .collect()
}

fn period_buckets(
    record: &SkillRecord,
    unit: Granularity,
    from: Date,
    to: Date,
) -> BTreeMap<Date, Bucket> {
    let mut result = BTreeMap::<Date, Bucket>::new();
    for (level, buckets) in [
        (Granularity::Day, &record.daily),
        (Granularity::Month, &record.monthly),
        (Granularity::Year, &record.yearly),
    ] {
        if level > unit {
            continue;
        }
        for (key, bucket) in buckets {
            if let Some(date) = level.date(key).map(|date| unit.start(date))
                && (from..=to).contains(&date)
            {
                result.entry(date).or_default().add(bucket);
            }
        }
    }
    result
}

fn series(entries: &[(Row, BTreeMap<Date, Bucket>)], periods: &[Date]) -> Vec<Series> {
    let mut ranked: Vec<_> = entries
        .iter()
        .filter(|(row, _)| row.usage.uses() != 0)
        .collect();
    ranked.sort_by_key(|(row, _)| {
        (
            Reverse(row.usage.automatic),
            Reverse(row.usage.pinned),
            row.id,
        )
    });
    let mut result: Vec<_> = ranked
        .iter()
        .take(TOP_SERIES)
        .map(|(row, buckets)| series_of(Some(row.id), row.name.clone(), buckets, periods))
        .collect();
    if ranked.len() > TOP_SERIES {
        let mut others = BTreeMap::<Date, Bucket>::new();
        for (_, buckets) in ranked.into_iter().skip(TOP_SERIES) {
            for (&date, bucket) in buckets {
                others.entry(date).or_default().add(bucket);
            }
        }
        result.push(series_of(None, "Others".into(), &others, periods));
    }
    result
}

fn series_of(
    id: Option<u64>,
    name: String,
    buckets: &BTreeMap<Date, Bucket>,
    periods: &[Date],
) -> Series {
    let counts = periods.iter().map(|date| {
        buckets
            .get(date)
            .map_or((0, 0), |bucket| (bucket.automatic, bucket.pinned))
    });
    let (automatic, pinned) = counts.unzip();
    Series {
        id,
        name,
        automatic,
        pinned,
    }
}

fn growth(
    history: &BTreeMap<Date, u64>,
    current: u64,
    from: Date,
    today: Date,
) -> (Option<f64>, Option<f64>) {
    let baseline = history
        .range(from..=today)
        .next()
        .or_else(|| history.range(..from).next_back());
    let Some((&date, &tokens)) = baseline else {
        return (None, None);
    };
    let delta = current as f64 - tokens as f64;
    let days = today.since(date).ok().map_or(0, |span| span.get_days());
    (
        (days >= 7).then(|| delta * DAYS_PER_MONTH / f64::from(days)),
        (tokens != 0).then(|| delta * 100.0 / tokens as f64),
    )
}

fn hints(
    history: &BTreeMap<Date, u64>,
    last_used: Option<Date>,
    current: u64,
    today: Date,
) -> Vec<Hint> {
    let mut result = Vec::new();
    let unused = today.saturating_sub(Span::new().days(90));
    let last_used = last_used.or_else(|| history.first_key_value().map(|(&date, _)| date));
    if last_used.is_some_and(|date| date <= unused) {
        result.push(Hint::Unused);
    }
    let baseline = today.saturating_sub(Span::new().days(30));
    if history
        .range(..=baseline)
        .next_back()
        .is_some_and(|(_, &tokens)| tokens != 0 && current > tokens.saturating_mul(2))
    {
        result.push(Hint::Growing);
    }
    if current > 1_000 {
        result.push(Hint::Large);
    }
    result
}
