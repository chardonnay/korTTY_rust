//! The log table uses the same retained, masked steps as the Agent activity actions.

use std::time::{Duration, Instant, SystemTime};

use iced::widget::{Id, button, column, container, mouse_area, row, scrollable, text};
use iced::{Color, Element, Font, Length, Task, mouse};
use jiff::{Timestamp, tz::TimeZone};

use crate::terminal_agent::design::inline::{
    Kind, State as TranscriptState, Step, StepState, Transcript,
};

use super::super::Message as AgentMessage;
use super::{Palette, history::Activity, presentation};

mod keyboard;

const FLASH_DURATION: Duration = Duration::from_millis(900);
const SPINNER_INTERVAL: u64 = 120;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Row {
    Step(u64),
    Answer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Column {
    Time,
    Kind,
    Step,
    Result,
    Duration,
    Tokens,
}

impl Column {
    const ALL: [Self; 6] = [
        Self::Time,
        Self::Kind,
        Self::Step,
        Self::Result,
        Self::Duration,
        Self::Tokens,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Time => "Time",
            Self::Kind => "Kind",
            Self::Step => "Step",
            Self::Result => "Result",
            Self::Duration => "Duration",
            Self::Tokens => "Tokens",
        }
    }

    fn width(self) -> Length {
        Length::Fixed(match self {
            Self::Time | Self::Kind | Self::Duration | Self::Tokens => 86.0,
            Self::Result => 108.0,
            Self::Step => 240.0,
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Select(Row),
    Open(Row),
    Close,
    Sort(Column),
    Navigate(Key),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Key {
    Previous,
    Next,
    First,
    Last,
    Open,
    Close,
    Leave,
}

pub(super) struct State {
    viewport: Id,
    selected: Option<Row>,
    detail: Option<Row>,
    sort: Option<(Column, bool)>,
    first_seen: Vec<(Row, Instant)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            viewport: Id::unique(),
            selected: None,
            detail: None,
            sort: None,
            first_seen: Vec::new(),
        }
    }
}

impl State {
    pub(super) fn sync(&mut self, transcript: &Transcript, now: Instant) {
        self.first_seen.retain(|(row, _)| match *row {
            Row::Step(id) => transcript.steps.iter().any(|step| step.id == id),
            Row::Answer => !transcript.final_answer.is_empty(),
        });
        let rows = transcript.steps.iter().map(|step| Row::Step(step.id));
        let answer = (!transcript.final_answer.is_empty()).then_some(Row::Answer);
        for row in rows.chain(answer) {
            if self.seen_at(row).is_none() {
                self.first_seen.push((row, now));
            }
        }
    }

    fn seen_at(&self, row: Row) -> Option<Instant> {
        self.first_seen
            .iter()
            .find(|(key, _)| *key == row)
            .map(|(_, at)| *at)
    }

    pub(super) fn refresh_interval(
        &self,
        transcript: &Transcript,
        now: Instant,
        active: bool,
        spinning: bool,
        reduced_motion: bool,
    ) -> Option<u64> {
        if !reduced_motion
            && self
                .first_seen
                .iter()
                .any(|(_, at)| now.saturating_duration_since(*at) < FLASH_DURATION)
        {
            Some(16)
        } else if active
            && transcript
                .steps
                .iter()
                .any(|step| step.state == StepState::Running && step.started_at.is_some())
        {
            Some(if spinning && !reduced_motion {
                SPINNER_INTERVAL
            } else {
                1_000
            })
        } else {
            None
        }
    }

    fn flash(&self, row: Row, now: Instant) -> f32 {
        self.seen_at(row).map_or(0.0, |at| {
            let progress =
                now.saturating_duration_since(at).as_secs_f32() / FLASH_DURATION.as_secs_f32();
            (1.0 - progress).max(0.0) * 0.5
        })
    }

    fn spinner(&self, row: Row, now: Instant) -> &'static str {
        let frame = self.seen_at(row).map_or(0, |at| {
            now.saturating_duration_since(at).as_millis() / u128::from(SPINNER_INTERVAL)
        });
        SPINNER[(frame % SPINNER.len() as u128) as usize]
    }

    pub(super) fn update(&mut self, message: Message) {
        match message {
            Message::Select(row) => {
                self.selected = Some(row);
                if self.detail != Some(row) {
                    self.detail = None;
                }
            }
            Message::Open(row) => {
                self.selected = Some(row);
                self.detail = Some(row);
            }
            Message::Close => self.detail = None,
            Message::Sort(column) => {
                let ascending = !matches!(self.sort, Some((current, true)) if current == column);
                self.sort = Some((column, ascending));
            }
            Message::Navigate(_) => {}
        }
    }

    fn navigate(&mut self, transcript: &Transcript, at: Instant, key: Key) -> Option<Row> {
        if matches!(key, Key::Close | Key::Leave) {
            self.detail = None;
            return None;
        }
        let mut order: Vec<_> = self
            .rows(transcript, at)
            .into_iter()
            .map(|step| Row::Step(step.id))
            .collect();
        order.extend((!transcript.final_answer.is_empty()).then_some(Row::Answer));
        let last = order.len().checked_sub(1)?;
        let current = self
            .selected
            .and_then(|selected| order.iter().position(|row| *row == selected));
        let next = match key {
            Key::Previous => current.map_or(last, |index| index.saturating_sub(1)),
            Key::Next => current.map_or(0, |index| (index + 1).min(last)),
            Key::First => 0,
            Key::Last => last,
            Key::Open => current.unwrap_or(0),
            Key::Close | Key::Leave => return None,
        };
        let row = order[next];
        self.update(if matches!(key, Key::Open) {
            Message::Open(row)
        } else {
            Message::Select(row)
        });
        Some(row)
    }

    fn row_id(&self, row: Row) -> Id {
        Id::from(format!("{:?}/log/{row:?}", self.viewport))
    }

    fn rows<'a>(&self, transcript: &'a Transcript, now: Instant) -> Vec<&'a Step> {
        let mut steps = transcript.steps.iter().collect::<Vec<_>>();
        if let Some((column, ascending)) = self.sort {
            steps.sort_by(|left, right| {
                let ordering = match column {
                    Column::Time => left.recorded_at.cmp(&right.recorded_at),
                    Column::Kind => kind(left).cmp(kind(right)),
                    Column::Step => title(left).cmp(title(right)),
                    Column::Result => left
                        .exit_code
                        .cmp(&right.exit_code)
                        .then_with(|| state_label(left).cmp(state_label(right))),
                    Column::Duration => left.elapsed_at(now).cmp(&right.elapsed_at(now)),
                    Column::Tokens => left.tokens.cmp(&right.tokens),
                };
                if ascending {
                    ordering
                } else {
                    ordering.reverse()
                }
            });
        }
        steps
    }

    fn detail<'a>(&self, transcript: &'a Transcript) -> Option<Detail<'a>> {
        match self.detail? {
            Row::Step(id) => transcript
                .steps
                .iter()
                .find(|step| step.id == id)
                .map(Detail::Step),
            Row::Answer if !transcript.final_answer.is_empty() => {
                Some(Detail::Answer(&transcript.final_answer))
            }
            Row::Answer => None,
        }
    }
}

impl super::Run {
    pub(in crate::workspace::terminal_agent) fn navigate_agent_log(
        &mut self,
        key: Key,
    ) -> Task<super::Desktop> {
        if self.design.history != crate::terminal_agent::design::History::LogTable {
            return Task::none();
        }
        let Some(history) = &self.inline else {
            return Task::none();
        };
        let at = self.presentation.finish_time().unwrap_or_else(Instant::now);
        let table = &mut self.presentation.log_table;
        table
            .navigate(history.status_transcript(), at, key)
            .map_or_else(Task::none, |row| {
                crate::workspace::coding_agents::roster_scroll::reveal_row(
                    table.viewport.clone(),
                    table.row_id(row),
                )
            })
    }
}

impl super::Workspace {
    pub(in crate::workspace) fn agent_log_keyboard<'a>(
        &'a self,
        window: iced::window::Id,
        content: Element<'a, super::Desktop>,
    ) -> Element<'a, super::Desktop> {
        if self.windows.contains_key(&window) || self.terminal_agents.window_run(window).is_some() {
            keyboard::wrap(content, &self.terminal_agents)
        } else {
            content
        }
    }
}

enum Detail<'a> {
    Step(&'a Step),
    Answer(&'a str),
}

fn event(message: Message) -> AgentMessage {
    AgentMessage::Design(presentation::Message::LogTable(message))
}

fn kind(step: &Step) -> &'static str {
    match step.kind {
        Kind::Thinking => "AI",
        Kind::Action => "Command",
        Kind::Question => "Question",
        Kind::Error => "Error",
        Kind::Message => "Message",
    }
}

fn title(step: &Step) -> &str {
    step.command.as_deref().unwrap_or(&step.title)
}

fn result(step: &Step, spinner: &str) -> String {
    if let Some(code) = step.exit_code {
        return format!("Exit {code}");
    }
    if step.state == StepState::Running {
        return format!("{spinner} Running");
    }
    state_label(step).into()
}

fn state_label(step: &Step) -> &'static str {
    match step.state {
        StepState::Running => "Running",
        StepState::Completed => "Completed",
        StepState::Failed => "Failed",
        StepState::Cancelled => "Cancelled",
    }
}

fn clock(time: Option<SystemTime>, zone: &TimeZone) -> String {
    time.and_then(|time| Timestamp::try_from(time).ok())
        .map_or_else(
            || "—".into(),
            |time| time.to_zoned(zone.clone()).strftime("%H:%M:%S").to_string(),
        )
}

fn timestamped_values(step: &Step, zone: &TimeZone, spinner: &str, now: Instant) -> [String; 6] {
    [
        clock(step.recorded_at, zone),
        kind(step).into(),
        super::history::summary(title(step)),
        result(step, spinner),
        step.elapsed_at(now)
            .map_or_else(|| "—".into(), |ms| format!("{:.1} s", ms as f64 / 1000.0)),
        step.tokens
            .map_or_else(|| "—".into(), |tokens| tokens.to_string()),
    ]
}

fn cells(values: [String; 6], size: f32) -> Element<'static, AgentMessage> {
    let mut cells = row![].spacing(4);
    for (column, value) in Column::ALL.into_iter().zip(values) {
        cells = cells.push(
            container(
                text(value)
                    .font(Font::MONOSPACE)
                    .size(size)
                    .wrapping(text::Wrapping::None),
            )
            .width(column.width())
            .clip(true),
        );
    }
    cells.into()
}

fn entry(
    row: Row,
    state: &State,
    values: [String; 6],
    size: f32,
    flash: f32,
    accent: Option<Color>,
) -> Element<'static, AgentMessage> {
    let selected = state.selected == Some(row);
    mouse_area(
        container(cells(values, size))
            .id(state.row_id(row))
            .padding([2, 5])
            .style(move |theme: &iced::Theme| {
                let mut style = if selected {
                    container::bordered_box(theme)
                } else {
                    container::transparent(theme)
                };
                if flash > 0.0 {
                    style.background = Some(
                        accent
                            .unwrap_or_else(|| theme.palette().primary)
                            .scale_alpha(flash)
                            .into(),
                    );
                }
                style
            }),
    )
    .interaction(mouse::Interaction::Pointer)
    .on_press(event(Message::Select(row)))
    .on_double_click(event(Message::Open(row)))
    .on_right_press(event(Message::Open(row)))
    .into()
}

fn details<'a>(run: Activity<'a>, detail: Detail<'a>) -> Element<'a, AgentMessage> {
    let (title, body, actions) = match detail {
        Detail::Step(step) => (
            super::history::summary(title(step)),
            if step.output.is_empty() {
                step.detail.clone()
            } else {
                step.output.join("\n")
            },
            run.live_run.map(|run| super::actions::step_menu(run, step)),
        ),
        Detail::Answer(answer) => ("Answer".into(), answer.into(), None),
    };
    container(
        column![
            text(title).width(Length::Fill),
            row![
                actions.map(|actions| container(actions).id("agent-log-step-actions")),
                button("Close details").on_press(event(Message::Close))
            ]
            .spacing(5)
            .wrap(),
            text(if body.is_empty() {
                "No output recorded.".into()
            } else {
                body
            })
            .size(run.activity_font),
        ]
        .spacing(4),
    )
    .width(Length::Fill)
    .padding(6)
    .style(container::bordered_box)
    .into()
}

pub(super) fn view<'a>(
    run: Activity<'a>,
    transcript: &'a Transcript,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'a, AgentMessage> {
    let size = run.activity_font - 1.0;
    let state = &run.presentation.log_table;
    let now = Instant::now();
    let duration_at = run
        .live_run
        .and_then(|run| run.presentation.finish_time())
        .unwrap_or(now);
    let flash = |row| {
        if reduced_motion || run.live_run.is_some_and(|run| run.reopened_history) {
            0.0
        } else {
            state.flash(row, now)
        }
    };
    let spinning = !reduced_motion
        && run
            .live_run
            .map_or(transcript.state == TranscriptState::Running, |run| {
                run.active() && !run.paused && !run.cancel.requested()
            });
    let accent = palette.map(|palette| palette.accent.color());
    let zone = TimeZone::system();
    let mut headings = row![].spacing(4).padding([2, 5]);
    for column in Column::ALL {
        let label = match state.sort {
            Some((current, ascending)) if current == column => {
                format!("{} {}", column.label(), if ascending { "↑" } else { "↓" })
            }
            _ => column.label().into(),
        };
        headings = headings.push(
            button(text(label).size(size))
                .width(column.width())
                .padding(0)
                .style(button::text)
                .on_press(event(Message::Sort(column))),
        );
    }
    let mut rows = column![headings].spacing(1);
    for step in state.rows(transcript, duration_at) {
        let row = Row::Step(step.id);
        rows = rows.push(entry(
            row,
            state,
            timestamped_values(
                step,
                &zone,
                if spinning {
                    state.spinner(row, now)
                } else {
                    "…"
                },
                duration_at,
            ),
            size,
            flash(row),
            accent,
        ));
    }
    if !transcript.final_answer.is_empty() {
        rows = rows.push(entry(
            Row::Answer,
            state,
            [
                clock(transcript.final_answer_at, &zone),
                "Answer".into(),
                super::history::summary(&transcript.final_answer),
                "—".into(),
                "—".into(),
                "—".into(),
            ],
            size,
            flash(Row::Answer),
            accent,
        ));
    }
    let table = scrollable(rows)
        .id(state.viewport.clone())
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .width(Length::Fill)
        .height(Length::FillPortion(2));
    let detail = state
        .detail(transcript)
        .map(|detail| scrollable(details(run, detail)).height(Length::FillPortion(1)));
    let open = state.selected.filter(|row| match row {
        Row::Step(id) => transcript.steps.iter().any(|step| step.id == *id),
        Row::Answer => !transcript.final_answer.is_empty(),
    });
    let open = open
        .filter(|_| detail.is_none())
        .map(|row| button("Open details").on_press(event(Message::Open(row))));
    column![table, open, detail]
        .height(Length::Fill)
        .spacing(4)
        .into()
}

#[cfg(test)]
mod tests {
    use iced::{Event, Point, Renderer, Size, advanced::clipboard};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use super::*;

    #[test]
    fn agent_log_table_keeps_exact_output_when_sorted_and_ignores_evicted_rows() {
        let now = Instant::now();
        let mut transcript = Transcript {
            final_answer: "Finished answer".into(),
            steps: vec![
                Step {
                    id: 17,
                    kind: Kind::Action,
                    command: Some("fixture command".into()),
                    output: vec!["exact output".into()],
                    exit_code: Some(3),
                    elapsed_ms: Some(1500),
                    state: StepState::Failed,
                    recorded_at: Some(SystemTime::UNIX_EPOCH),
                    ..Step::default()
                },
                Step {
                    id: 18,
                    kind: Kind::Thinking,
                    elapsed_ms: Some(1200),
                    tokens: Some(2048),
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        assert_eq!(
            timestamped_values(&transcript.steps[0], &TimeZone::UTC, "…", now),
            [
                "00:00:00",
                "Command",
                "fixture command",
                "Exit 3",
                "1.5 s",
                "—"
            ]
        );
        assert_eq!(
            timestamped_values(&transcript.steps[1], &TimeZone::UTC, "…", now)[5],
            "2048"
        );
        let mut state = State::default();
        state.update(Message::Open(Row::Step(17)));
        state.update(Message::Sort(Column::Duration));
        assert_eq!(state.rows(&transcript, now)[0].id, 18);
        assert!(
            matches!(state.detail(&transcript), Some(Detail::Step(step)) if step.output == ["exact output"])
        );
        state.update(Message::Sort(Column::Duration));
        assert_eq!(state.rows(&transcript, now)[0].id, 17);
        transcript.steps.remove(0);
        assert!(state.detail(&transcript).is_none());
        state.update(Message::Open(Row::Answer));
        assert!(matches!(
            state.detail(&transcript),
            Some(Detail::Answer("Finished answer"))
        ));
    }

    #[test]
    fn agent_log_table_double_click_opens_the_exact_row() {
        let step = Row::Step(17);
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut ui = UserInterface::build(
            entry(
                step,
                &State::default(),
                Column::ALL.map(|column| column.label().into()),
                12.0,
                0.0,
                None,
            ),
            Size::new(800.0, 140.0),
            Cache::default(),
            &mut renderer,
        );
        let position = Point::new(12.0, 12.0);
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let mut messages = Vec::new();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position }),
                press.clone(),
                release.clone(),
                press,
                release,
            ],
            mouse::Cursor::Available(position),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(
            messages.as_slice(),
            [
                AgentMessage::Design(presentation::Message::LogTable(Message::Select(Row::Step(
                    17
                )))),
                AgentMessage::Design(presentation::Message::LogTable(Message::Select(Row::Step(
                    17
                )))),
                AgentMessage::Design(presentation::Message::LogTable(Message::Open(Row::Step(
                    17
                ))))
            ]
        ));
    }

    #[test]
    fn agent_log_table_flashes_once_per_row_and_animates_only_running_steps() {
        let now = Instant::now();
        let mut transcript = Transcript {
            steps: vec![Step {
                id: 17,
                started_at: Some(now),
                state: StepState::Running,
                ..Step::default()
            }],
            ..Transcript::default()
        };
        let row = Row::Step(17);
        let mut state = State::default();
        state.sync(&transcript, now);
        assert_eq!(state.flash(row, now), 0.5);
        assert_eq!(
            state.refresh_interval(&transcript, now, true, true, false),
            Some(16)
        );
        let next_frame = now + Duration::from_millis(SPINNER_INTERVAL);
        assert_ne!(state.spinner(row, now), state.spinner(row, next_frame));
        assert_eq!(result(&transcript.steps[0], "…"), "… Running");

        let expired = now + FLASH_DURATION;
        state.sync(&transcript, expired);
        assert_eq!(state.flash(row, expired), 0.0);
        assert_eq!(
            state.refresh_interval(&transcript, expired, true, true, false),
            Some(120)
        );
        assert_eq!(
            state.refresh_interval(&transcript, expired, true, false, false),
            Some(1_000)
        );
        assert_eq!(
            state.refresh_interval(&transcript, now, true, true, true),
            Some(1_000)
        );
        transcript.steps[0].state = StepState::Completed;
        state.sync(&transcript, expired);
        assert_eq!(
            state.refresh_interval(&transcript, expired, true, true, false),
            None
        );

        transcript.steps.clear();
        transcript.final_answer = "Finished".into();
        state.sync(&transcript, expired);
        assert!(state.seen_at(row).is_none());
        assert_eq!(state.flash(Row::Answer, expired), 0.5);
    }

    #[test]
    fn agent_log_table_uses_the_same_duration_for_cells_and_sorting() {
        let now = Instant::now();
        let mut transcript = Transcript {
            steps: vec![
                Step {
                    id: 1,
                    state: StepState::Running,
                    started_at: Some(now),
                    ..Step::default()
                },
                Step {
                    id: 2,
                    elapsed_ms: Some(1_500),
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        let mut state = State::default();
        state.update(Message::Sort(Column::Duration));
        assert_eq!(state.rows(&transcript, now)[0].id, 1);
        let later = now + Duration::from_secs(2);
        assert_eq!(state.rows(&transcript, later)[0].id, 2);
        assert_eq!(
            timestamped_values(&transcript.steps[0], &TimeZone::UTC, "…", later)[4],
            "2.0 s"
        );

        let completed = &mut transcript.steps[0];
        completed.state = StepState::Completed;
        completed.elapsed_ms = Some(0);
        assert_eq!(
            timestamped_values(completed, &TimeZone::UTC, "…", later)[4],
            "0.0 s"
        );
        assert_eq!(state.rows(&transcript, later)[0].id, 1);
        assert_eq!(
            timestamped_values(&Step::default(), &TimeZone::UTC, "…", later)[4],
            "—"
        );
    }
}
