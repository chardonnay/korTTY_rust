//! The local AI skill usage and size report.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, canvas, checkbox, column, pick_list, row, scrollable, text};
use iced::{Element, Length, Point, Rectangle, Renderer, Size, Task, Theme, mouse};

use crate::ai_manager::Configuration;
use crate::ai_manager::profiles::picker::DismissOnEscape as _;

use super::super::Store;
use super::super::statistics::{self, Current, Granularity, Hint, Ledger, Range, Report};
use super::super::usage::allowed_now;

#[derive(Clone)]
pub(crate) enum Message {
    Open,
    Close,
    Refresh,
    Granularity(Granularity),
    Periods(PeriodChoice),
    IncludePinned(bool),
    Select(Option<u64>),
    Reset,
    ConfirmReset,
    CancelReset,
    Loaded(u64, Result<Arc<Snapshot>, String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PeriodChoice {
    amount: usize,
    label: &'static str,
}

impl std::fmt::Display for PeriodChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label)
    }
}

pub(crate) struct Snapshot {
    ledger: Ledger,
    skills: Vec<Current>,
}

pub(super) struct Panel {
    root: PathBuf,
    granularity: Granularity,
    periods: PeriodChoice,
    include_pinned: bool,
    selected: Option<u64>,
    snapshot: Option<Arc<Snapshot>>,
    report: Option<Report>,
    pending: Option<u64>,
    confirm_reset: bool,
    notice: Option<String>,
}

impl Panel {
    pub(super) fn open(root: PathBuf, ticket: &mut u64) -> (Self, Task<Message>) {
        let mut panel = Self {
            root,
            granularity: Granularity::Day,
            periods: period_choices(Granularity::Day)[0],
            include_pinned: true,
            selected: None,
            snapshot: None,
            report: None,
            pending: None,
            confirm_reset: false,
            notice: None,
        };
        let task = panel.load(ticket, false);
        (panel, task)
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn update(&mut self, event: Message, ticket: &mut u64) -> Task<Message> {
        match event {
            Message::Refresh if !self.busy() => return self.load(ticket, false),
            Message::Granularity(value) => {
                self.granularity = value;
                self.periods = period_choices(value)[0];
                self.refresh_report();
            }
            Message::Periods(value) if period_choices(self.granularity).contains(&value) => {
                self.periods = value;
                self.refresh_report();
            }
            Message::IncludePinned(value) => self.include_pinned = value,
            Message::Select(id) => self.selected = id,
            Message::Reset if !self.busy() => self.confirm_reset = true,
            Message::ConfirmReset if self.confirm_reset && !self.busy() => {
                self.confirm_reset = false;
                return self.load(ticket, true);
            }
            Message::CancelReset => self.confirm_reset = false,
            Message::Loaded(generation, result) if self.pending == Some(generation) => {
                self.pending = None;
                match result {
                    Ok(snapshot) => {
                        self.snapshot = Some(snapshot);
                        self.refresh_report();
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn load(&mut self, ticket: &mut u64, reset: bool) -> Task<Message> {
        *ticket = ticket.wrapping_add(1);
        let generation = *ticket;
        self.pending = Some(generation);
        self.notice = None;
        let root = self.root.clone();
        Task::perform(
            async move { snapshot(&root, reset).map(Arc::new) },
            move |result| Message::Loaded(generation, result),
        )
    }

    fn refresh_report(&mut self) {
        self.report = self.snapshot.as_ref().map(|snapshot| {
            snapshot.ledger.report(
                Range {
                    granularity: self.granularity,
                    periods: self.periods.amount,
                },
                &snapshot.skills,
            )
        });
        if self.selected.is_some_and(|id| {
            self.report
                .as_ref()
                .is_none_or(|report| !report.rows.iter().any(|row| row.id == id))
        }) {
            self.selected = None;
        }
    }

    pub(super) fn view(&self, store: &Store, managed: Option<bool>) -> Element<'_, Message> {
        let header = row![
            text("Skill statistics").size(20).width(Length::Fill),
            button("Close").on_press(Message::Close),
        ]
        .spacing(8);
        let controls = row![
            pick_list(
                [Granularity::Day, Granularity::Month, Granularity::Year],
                Some(self.granularity),
                Message::Granularity,
            )
            .dismiss_on_escape(),
            pick_list(
                period_choices(self.granularity),
                Some(self.periods),
                Message::Periods,
            )
            .dismiss_on_escape(),
            checkbox(self.include_pinned)
                .label("Include pinned in chart")
                .on_toggle(Message::IncludePinned),
            button("Refresh").on_press_maybe((!self.busy()).then_some(Message::Refresh)),
            button("Clear statistics…").on_press_maybe((!self.busy()).then_some(Message::Reset)),
        ]
        .spacing(8)
        .wrap();
        let body = if let Some(report) = &self.report {
            self.report_view(report)
        } else {
            column![text(if self.busy() {
                "Reading local skill statistics…"
            } else {
                "No skill statistics are loaded."
            })]
        };
        column![
            header,
            controls,
            (!store.statistics_enabled(managed))
                .then(
                    || text("Recording is disabled. Saved statistics remain available.").size(13)
                ),
            self.confirm_reset.then(|| column![
                text("Clear saved usage counts and skill size history for this profile?"),
                row![
                    button("Clear statistics").on_press(Message::ConfirmReset),
                    button("Cancel").on_press(Message::CancelReset),
                ]
                .spacing(8)
            ]
            .spacing(4)),
            self.notice.as_deref().map(|message| text(message).size(13)),
            scrollable(body).height(Length::Fill),
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    fn report_view<'a>(&'a self, report: &'a Report) -> iced::widget::Column<'a, Message> {
        let mut body = column![
            text(format!(
                "{} automatic · {} pinned · {} tokens",
                report.total.automatic, report.total.pinned, report.total.tokens
            ))
            .size(15)
        ]
        .spacing(10);
        if let Some(id) = self.selected {
            let series = report.series.iter().find(|series| series.id == Some(id));
            let fallback = series
                .is_none()
                .then(|| {
                    self.snapshot.as_ref()?.ledger.skill_series(
                        id,
                        Range {
                            granularity: self.granularity,
                            periods: self.periods.amount,
                        },
                    )
                })
                .flatten();
            if let Some(series) = series.or(fallback.as_ref()) {
                body = body.push(text(series.name.clone()).size(16)).push(chart(
                    &series.automatic,
                    self.include_pinned.then_some(series.pinned.as_slice()),
                    &report.periods,
                ));
            }
        } else {
            let mut automatic = vec![0_u64; report.periods.len()];
            let mut pinned = vec![0_u64; report.periods.len()];
            for series in &report.series {
                for (total, count) in automatic.iter_mut().zip(&series.automatic) {
                    *total = total.saturating_add(*count);
                }
                for (total, count) in pinned.iter_mut().zip(&series.pinned) {
                    *total = total.saturating_add(*count);
                }
            }
            body = body.push(text("All skills").size(16)).push(chart(
                &automatic,
                self.include_pinned.then_some(pinned.as_slice()),
                &report.periods,
            ));
        }
        if report.total.automatic == 0 && report.total.pinned == 0 {
            body = body.push(
                text(if report.has_usage {
                    "No successful skill requests have been recorded in this period."
                } else {
                    "No successful skill requests have been recorded."
                })
                .size(13),
            );
        }
        let average = report
            .average_size_tokens
            .map_or_else(|| "—".into(), |value| format!("{value:.1}"));
        let growth = report
            .growth_tokens_per_month
            .map_or_else(|| "—".into(), |value| format!("{value:+.1}"));
        body = body.push(
            text(format!(
                "Average skill size {average} tokens · Total growth {growth} tokens per month"
            ))
            .size(13),
        );
        if let Some(selected) = self.selected
            && let Some(skill) = report.rows.iter().find(|skill| skill.id == selected)
        {
            let surfaces = skill
                .usage
                .surfaces
                .iter()
                .map(|(surface, uses)| format!("{surface} {uses}"))
                .collect::<Vec<_>>()
                .join(" · ");
            let current = skill
                .current_tokens
                .map_or_else(|| "—".into(), |tokens| tokens.to_string());
            let avg = skill
                .average_tokens
                .map_or_else(|| "—".into(), |tokens| format!("{tokens:.1}"));
            let status = if !skill.current {
                "Removed"
            } else if skill.enabled {
                "Enabled"
            } else {
                "Disabled"
            };
            let last_used = skill.last_used.as_deref().unwrap_or("—");
            let hints = skill
                .hints
                .iter()
                .map(|hint| match hint {
                    Hint::Unused => "Unused",
                    Hint::Growing => "Growing",
                    Hint::Large => "Large",
                })
                .collect::<Vec<_>>()
                .join(" · ");
            body = body.push(text(format!(
                "{status} · {current} current tokens · {avg} tokens per use · {} learned rules · Last used {last_used}",
                skill.learned_rules
            )).size(13));
            if !surfaces.is_empty() {
                body = body.push(text(surfaces).size(13));
            }
            if !hints.is_empty() {
                body = body.push(text(hints).size(13));
            }
            if let Some(growth) = skill.growth_tokens_per_month {
                let percent = skill
                    .growth_percent
                    .map_or_else(String::new, |percent| format!(" · {percent:+.1}%"));
                body = body.push(
                    text(format!(
                        "Size growth {growth:+.1} tokens per month{percent}"
                    ))
                    .size(13),
                );
            }
            if !skill.history.is_empty() {
                let values: Vec<_> = skill.history.iter().map(|point| point.tokens).collect();
                let dates: Vec<_> = skill
                    .history
                    .iter()
                    .map(|point| point.date.clone())
                    .collect();
                body = body
                    .push(text("Skill size history").size(15))
                    .push(chart(&values, None, &dates));
            }
        }
        body = body.push(
            button("All skills").on_press_maybe(self.selected.map(|_| Message::Select(None))),
        );
        for skill in &report.rows {
            body = body.push(
                row![
                    button(text(&skill.name).size(13))
                        .style(if self.selected == Some(skill.id) {
                            button::primary
                        } else {
                            button::text
                        })
                        .width(Length::Fill)
                        .on_press(Message::Select(Some(skill.id))),
                    text(format!(
                        "{} automatic · {} pinned · {} tokens",
                        skill.usage.automatic, skill.usage.pinned, skill.usage.tokens
                    ))
                    .size(13),
                ]
                .spacing(8)
                .wrap(),
            );
        }
        body
    }
}

fn snapshot(root: &Path, reset: bool) -> Result<Snapshot, String> {
    super::super::usage::flush();
    if reset {
        statistics::reset(root)
            .map_err(|error| format!("Could not clear skill statistics ({error})."))?;
    }
    let config = Configuration::load_from(root)
        .map_err(|error| format!("Could not load AI settings ({error})."))?;
    let mut ledger = statistics::read(root)
        .map_err(|error| format!("Could not read skill statistics ({error})."))?;
    let skills = config.skills.current_statistics(ledger.tokenizer());
    if !reset && config.skill_statistics_allowed(&crate::clipboard_policy::ClipboardPolicy::load())
    {
        let sizes: Vec<_> = skills.iter().map(|skill| skill.skill.clone()).collect();
        if statistics::observe(root, &sizes, || allowed_now(root)).is_ok() {
            ledger = statistics::read(root).unwrap_or(ledger);
        }
    }
    Ok(Snapshot { ledger, skills })
}

fn period_choices(granularity: Granularity) -> [PeriodChoice; 3] {
    let pairs = match granularity {
        Granularity::Day => [
            (30, "Last 30 days"),
            (90, "Last 90 days"),
            (400, "Last 400 days"),
        ],
        Granularity::Month => [
            (12, "Last 12 months"),
            (24, "Last 24 months"),
            (60, "Last 60 months"),
        ],
        Granularity::Year => [(0, "All years"), (5, "Last 5 years"), (10, "Last 10 years")],
    };
    pairs.map(|(amount, label)| PeriodChoice { amount, label })
}

fn chart<'a>(automatic: &[u64], pinned: Option<&[u64]>, dates: &[String]) -> Element<'a, Message> {
    let plot = Bars {
        automatic: automatic.to_vec(),
        pinned: pinned.map(<[u64]>::to_vec),
    };
    column![
        canvas::Canvas::new(plot).width(Length::Fill).height(100),
        row![
            text(dates.first().cloned().unwrap_or_default())
                .size(12)
                .width(Length::Fill),
            text(dates.last().cloned().unwrap_or_default()).size(12),
        ],
    ]
    .spacing(4)
    .into()
}

struct Bars {
    automatic: Vec<u64>,
    pinned: Option<Vec<u64>>,
}

impl canvas::Program<Message> for Bars {
    type State = ();

    fn draw(
        &self,
        _: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        if self.automatic.is_empty() {
            return vec![frame.into_geometry()];
        }
        let palette = theme.extended_palette();
        let pin = |index| {
            self.pinned
                .as_ref()
                .and_then(|counts| counts.get(index))
                .copied()
                .unwrap_or(0_u64)
        };
        let maximum = self
            .automatic
            .iter()
            .enumerate()
            .map(|(index, &value)| value.saturating_add(pin(index)))
            .max()
            .unwrap_or_default()
            .max(1) as f32;
        let width = bounds.width / self.automatic.len() as f32;
        for (index, &value) in self.automatic.iter().enumerate() {
            let base = bounds.height * value as f32 / maximum;
            let extra = bounds.height * pin(index) as f32 / maximum;
            let x = width * index as f32;
            frame.fill_rectangle(
                Point::new(x, bounds.height - base),
                Size::new(width * 0.8, base),
                palette.primary.strong.color,
            );
            frame.fill_rectangle(
                Point::new(x, bounds.height - base - extra),
                Size::new(width * 0.8, extra),
                palette.success.strong.color,
            );
        }
        vec![frame.into_geometry()]
    }
}
