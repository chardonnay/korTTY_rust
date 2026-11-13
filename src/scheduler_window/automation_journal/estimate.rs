use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use iced::Element;
use iced::widget::{button, column, text};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use kortty_jobs::JobSchedule;

use crate::local_ai::{UsageBudget, UsageCounters};
use crate::session_journal::automation::{Closing, Keep};
use crate::session_journal::{self, Summary, tree::SourceKind};

use super::super::Message;

const WEEK_DAYS: u64 = 7;
const MAX_WEEKLY_STARTS: u64 = (WEEK_DAYS + 1) * 24 * 60;
const MONTH_DAYS: u64 = 30;
// One full activity window (48 terminal lines at 25 tokens, plus 1,500 prompt
// tokens) and one closing prompt. Each example answer is 400 tokens.
const FALLBACK_INPUT: u64 = 4_200;
const FALLBACK_OUTPUT: u64 = 800;

#[derive(Clone, Eq, PartialEq)]
pub(in crate::scheduler_window) struct Subject {
    pub(in crate::scheduler_window) source: Option<String>,
    pub(in crate::scheduler_window) schedule: Option<JobSchedule>,
    pub(in crate::scheduler_window) targets: usize,
    pub(in crate::scheduler_window) mode: (Closing, Keep),
}

#[derive(Clone)]
pub struct Estimate(Vec<String>);

pub(crate) struct Request {
    pub(crate) ticket: u64,
    subject: Subject,
    zone: TimeZone,
    now: SystemTime,
}

#[derive(Default)]
pub(in crate::scheduler_window) struct Panel {
    ticket: u64,
    pending: Option<Subject>,
    ready: Option<(Subject, Estimate)>,
}

impl Panel {
    pub(in crate::scheduler_window) fn clear(&mut self) {
        self.pending = None;
        self.ready = None;
    }

    pub(in crate::scheduler_window) fn begin(
        &mut self,
        subject: Subject,
        zone: TimeZone,
        now: SystemTime,
    ) -> Option<Request> {
        if self.pending.is_some() {
            return None;
        }
        self.ticket = self.ticket.checked_add(1)?;
        self.pending = Some(subject.clone());
        self.ready = None;
        Some(Request {
            ticket: self.ticket,
            subject,
            zone,
            now,
        })
    }

    pub(in crate::scheduler_window) fn finish(&mut self, ticket: u64, estimate: Estimate) {
        if self.ticket == ticket
            && let Some(subject) = self.pending.take()
        {
            self.ready = Some((subject, estimate));
        }
    }

    pub(in crate::scheduler_window) fn view(
        &self,
        subject: Option<&Subject>,
    ) -> Element<'_, Message> {
        let mut view = column![].spacing(4);
        let Some(subject) = subject else {
            return view.into();
        };
        let pending = self.pending.is_some();
        view = view.push(
            button("Estimate journal summary usage")
                .on_press_maybe((!pending).then_some(Message::EstimateAutomationJournal)),
        );
        if pending {
            view = view.push(text("Reading journal usage and schedule…").size(12));
        } else if let Some((key, estimate)) = &self.ready
            && key == subject
        {
            for line in &estimate.0 {
                view = view.push(text(line).size(12));
            }
        }
        view.into()
    }
}

impl Estimate {
    pub(crate) fn unavailable() -> Self {
        Self(vec!["Journal summary usage is unavailable. Enable an allowed local journal AI profile to estimate it.".into()])
    }

    pub(crate) fn worker_stopped() -> Self {
        Self(vec!["The journal usage estimate could not finish.".into()])
    }

    pub(crate) fn swarm(
        root: &Path,
        source: &str,
        targets: usize,
        mode: (Closing, Keep),
        model: &str,
        budget: Option<&UsageBudget>,
    ) -> Self {
        let (_, lines) = per_run(
            root,
            Some((SourceKind::Swarm, source)),
            targets,
            mode,
            model,
            budget,
        );
        Self(lines)
    }

    pub(crate) fn lines(&self) -> &[String] {
        &self.0
    }
}

impl Request {
    pub(crate) fn calculate(
        self,
        root: &Path,
        model: &str,
        budget: Option<&UsageBudget>,
    ) -> Estimate {
        let (counts, mut lines) = per_run(
            root,
            self.subject
                .source
                .as_deref()
                .map(|source| (SourceKind::Job, source)),
            self.subject.targets,
            self.subject.mode,
            model,
            budget,
        );
        if let Some(weekly) = self
            .subject
            .schedule
            .as_ref()
            .and_then(|schedule| weekly_runs(schedule, self.now, &self.zone))
            .filter(|&count| count != 0)
        {
            let projected = counts.scale(MONTH_DAYS, WEEK_DAYS, weekly);
            lines.push(format!(
                "{weekly} scheduled starts in the next seven calendar days. At that cadence, about {} tokens per 30 days if every run is summarized.",
                projected.total(),
            ));
            if let Some(cost) = cost(budget, projected) {
                lines.push(format!("Estimated cost per 30 days · {cost}."));
            }
        }
        Estimate(lines)
    }
}

fn per_run(
    root: &Path,
    source: Option<(SourceKind, &str)>,
    targets: usize,
    mode: (Closing, Keep),
    model: &str,
    budget: Option<&UsageBudget>,
) -> (Counts, Vec<String>) {
    let historical = source.and_then(|(kind, source)| {
        let catalog = session_journal::catalog(root).ok()?;
        if catalog.limited {
            return None;
        }
        historical(&catalog.journals, kind, source)
    });
    let targets = targets as u64;
    let (counts, basis) = match historical {
        Some((counts, runs)) => (
            counts,
            format!("average of {runs} kept runs with reported usage"),
        ),
        None => (
            Counts {
                input: FALLBACK_INPUT * targets,
                output: FALLBACK_OUTPUT * targets,
            },
            "rough estimate for one activity window and one closing summary per target".into(),
        ),
    };
    let mut lines = vec![format!(
        "Journal model {model} · about {} tokens per summarized run ({basis}).",
        counts.total(),
    )];
    if matches!(mode, (Closing::Failures, _) | (_, Keep::Failures)) {
        lines.push("Only kept unsuccessful runs are eligible for these summaries.".into());
    }
    if let Some(cost) = cost(budget, counts) {
        lines.push(format!("Estimated cost per summarized run · {cost}."));
    }
    (counts, lines)
}

#[derive(Clone, Copy, Default, Debug, Eq, PartialEq)]
struct Counts {
    input: u64,
    output: u64,
}

impl Counts {
    fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }

    fn add(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
    }

    fn scale(self, numerator: u64, denominator: u64, multiplier: u64) -> Self {
        let scale = |value| {
            let scaled = u128::from(value) * u128::from(numerator) * u128::from(multiplier);
            u64::try_from((scaled + u128::from(denominator / 2)) / u128::from(denominator))
                .unwrap_or(u64::MAX)
        };
        Self {
            input: scale(self.input),
            output: scale(self.output),
        }
    }
}

fn historical(journals: &[Summary], kind: SourceKind, source: &str) -> Option<(Counts, u64)> {
    let mut targets = BTreeMap::<(&str, &str), Option<Counts>>::new();
    for journal in journals.iter().filter(|journal| {
        journal.source_kind == kind && journal.source_id.as_deref() == Some(source)
    }) {
        let Some(run) = journal.run_id.as_deref().filter(|value| !value.is_empty()) else {
            continue;
        };
        let tokens = Counts {
            input: journal.usage.prompt_tokens,
            output: journal.usage.completion_tokens,
        };
        let usage = (journal.ended_at.is_some()
            && !journal.usage_partial
            && tokens.total() != 0
            && tokens.total() == journal.usage.total_tokens)
            .then_some(tokens);
        let target = targets.entry((run, &journal.id)).or_insert(usage);
        if *target != usage {
            *target = None;
        }
    }
    let mut runs = BTreeMap::<&str, Option<Counts>>::new();
    for ((run, _), usage) in targets {
        let total = runs.entry(run).or_insert(Some(Counts::default()));
        match (total.as_mut(), usage) {
            (Some(total), Some(usage)) => total.add(usage),
            (_, None) => *total = None,
            (None, Some(_)) => {}
        }
    }
    let mut sum = Counts::default();
    let mut count = 0;
    for usage in runs.values().flatten() {
        sum.add(*usage);
        count += 1;
    }
    (count != 0).then(|| {
        let input = sum.input / count;
        let output = (sum.total() / count).saturating_sub(input);
        (Counts { input, output }, count)
    })
}

fn weekly_runs(schedule: &JobSchedule, now: SystemTime, zone: &TimeZone) -> Option<u64> {
    let limit = SystemTime::from(
        Timestamp::try_from(now)
            .ok()?
            .to_zoned(zone.clone())
            .checked_add(Span::new().days(WEEK_DAYS as i64))
            .ok()?
            .timestamp(),
    );
    let mut cursor = now;
    for count in 0..=MAX_WEEKLY_STARTS {
        let Some(next) = schedule.next_run_after(cursor, zone).ok()? else {
            return Some(count);
        };
        if next > limit {
            return Some(count);
        }
        cursor = next;
    }
    None
}

fn cost(budget: Option<&UsageBudget>, counts: Counts) -> Option<String> {
    let budget = budget?;
    let estimate = budget.estimated_cost(UsageCounters {
        input_tokens: counts.input,
        output_tokens: counts.output,
        ..UsageCounters::default()
    })?;
    if estimate.partial {
        return None;
    }
    Some(format!(
        "{:.4} {}",
        estimate.numerator as f64 / 1e12,
        budget.currency
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn journal(
        root: &Path,
        name: &str,
        id: &str,
        (source, run): (&str, &str),
        counts: Counts,
        partial: bool,
    ) -> Summary {
        let folder = root.join(name);
        fs::create_dir(&folder).unwrap();
        fs::write(
            folder.join("journal.xml"),
            format!(
                r#"<session-journal formatVersion="1" id="{id}"><meta><title>{name}</title>
            <sourceKind>JOB</sourceKind><sourceId>{source}</sourceId><runId>{run}</runId>
            <endedAt>2026-11-01T00:00:00Z</endedAt><aiCallCount>1</aiCallCount>
            <aiPromptTokens>{}</aiPromptTokens><aiCompletionTokens>{}</aiCompletionTokens>
            <aiTotalTokens>{}</aiTotalTokens><aiUsagePartial>{partial}</aiUsagePartial>
            </meta><entries/></session-journal>"#,
                counts.input,
                counts.output,
                counts.total(),
            ),
        )
        .unwrap();
        session_journal::read(&folder).unwrap().summary
    }

    #[test]
    fn estimate_uses_complete_kept_runs_with_exact_job_and_journal_identity() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let row = |folder, id, source, run, input, output, partial| {
            journal(
                &root,
                folder,
                id,
                (source, run),
                Counts { input, output },
                partial,
            )
        };
        let journals = [
            row("a", "a", "job", "run-1", 100, 10, false),
            row("a-copy", "a", "job", "run-1", 100, 10, false),
            row("b", "b", "job", "run-1", 200, 20, false),
            row("c", "c", "job", "run-2", 500, 50, false),
            row("d", "d", "another-job", "run-1", 9_000, 900, false),
            row("e", "e", "job", "run-3", 700, 70, false),
            row("f", "f", "job", "run-3", 0, 0, false),
            row("g", "g", "job", "run-4", 90, 9, true),
        ];
        assert_eq!(
            historical(&journals, SourceKind::Job, "job"),
            Some((
                Counts {
                    input: 400,
                    output: 40
                },
                2
            ))
        );
        assert_eq!(historical(&journals, SourceKind::Job, "missing"), None);
    }

    #[test]
    fn estimate_counts_coming_calendar_week_across_dst_and_disabled_schedule() {
        let zone = TimeZone::get("America/New_York").unwrap();
        let now = SystemTime::from("2026-03-06T17:00:00Z".parse::<Timestamp>().unwrap());
        let schedule = JobSchedule::default()
            .with_fixed_times(["11:30".parse().unwrap(), "12:30".parse().unwrap()]);
        assert_eq!(weekly_runs(&schedule, now, &zone), Some(14));
        assert_eq!(
            weekly_runs(&schedule.clone().with_enabled(false), now, &zone),
            Some(0)
        );
        let tomorrow = "2026-03-07".parse().unwrap();
        let one_day = schedule
            .with_active_dates(Some(tomorrow), Some(tomorrow))
            .unwrap();
        assert_eq!(weekly_runs(&one_day, now, &zone), Some(2));

        let fall_back = "2026-11-01".parse().unwrap();
        let hourly = JobSchedule::default()
            .with_interval_minutes(std::num::NonZeroU32::new(60))
            .with_active_dates(Some(fall_back), Some(fall_back))
            .unwrap();
        let before_fold = SystemTime::from("2026-10-31T17:00:00Z".parse::<Timestamp>().unwrap());
        assert_eq!(weekly_runs(&hourly, before_fold, &zone), Some(24));
    }

    #[test]
    fn estimate_quotes_only_complete_explicit_prices() {
        let counts = Counts {
            input: 2_000,
            output: 1_000,
        };
        let mut budget = UsageBudget::default();
        assert!(cost(Some(&budget), counts).is_none());
        budget.input_micros_per_million = Some(1_000_000);
        assert!(cost(Some(&budget), counts).is_none());
        budget.output_micros_per_million = Some(2_000_000);
        assert_eq!(cost(Some(&budget), counts).as_deref(), Some("0.0040 EUR"));
    }
}
