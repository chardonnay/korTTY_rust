//! The d08 rail projects typed steps into stations and animates only the visible rail.

use std::f32::consts::TAU;
use std::rc::Rc;
use std::time::{Duration, Instant};

use iced::widget::{
    button, canvas, column, container, responsive, row, scrollable, space, stack, text, tooltip,
};
use iced::{
    Alignment, Color, Element, Event, Font, Point, Rectangle, Renderer, Theme, font, mouse, window,
};

use crate::terminal_agent::design::Status;
use crate::terminal_agent::design::inline::{Kind, State, Step, StepState};
use crate::workspace::terminal_agent::design::{actions, log_table};

use super::top_bar::icon;
use super::{AgentMessage, Message, Palette, Tone, Trace, history, palette};

const BAR_HEIGHT: f32 = 52.0;
const HEIGHT: f32 = 32.0;
const RAIL_Y: f32 = 9.0;
const STATION_WIDTH: f32 = 58.0;
const TRAVEL: Duration = Duration::from_millis(650);
const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Id {
    Step(u64),
    Review,
    Answer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mark {
    Reached,
    Active,
    Attention,
    Failed,
    Cancelled,
    Pending,
}

impl Mark {
    fn current(self) -> bool {
        matches!(self, Self::Active | Self::Attention)
    }
}

struct Station<'a> {
    id: Id,
    label: String,
    detail: &'a str,
    mark: Mark,
}

impl<'a> Station<'a> {
    fn step(step: &'a Step, phase: &str) -> Option<Self> {
        let label = match step.kind {
            Kind::Thinking
                if step.state == StepState::Running && phase == super::REPAIRING_PHASE =>
            {
                phase.into()
            }
            Kind::Thinking => "Analysis".into(),
            Kind::Action | Kind::Error => {
                command_label(step.command.as_deref().unwrap_or(&step.title))
            }
            Kind::Question => "Question".into(),
            Kind::Message => return None,
        };
        let mark = match step.state {
            _ if step.kind == Kind::Error => Mark::Failed,
            StepState::Running if step.kind == Kind::Question => Mark::Attention,
            StepState::Running => Mark::Active,
            StepState::Completed => Mark::Reached,
            StepState::Failed => Mark::Failed,
            StepState::Cancelled => Mark::Cancelled,
        };
        Some(Self {
            id: Id::Step(step.id),
            label,
            detail: step.command.as_deref().unwrap_or(&step.title),
            mark,
        })
    }
}

fn command_label(command: &str) -> String {
    let mut words = command.split_whitespace();
    let first = words.next().unwrap_or("Command");
    let command = if first.eq_ignore_ascii_case("sudo") {
        words.next().unwrap_or(first)
    } else {
        first
    };
    if let Some(option) = words.next().filter(|word| word.starts_with('-'))
        && command.chars().count() + 1 + option.chars().count() <= 14
    {
        return format!("{command} {option}");
    }
    short_text(command, 14)
}

fn short_text(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value.chars().take(limit - 1).chain(['…']).collect()
}

fn stations<'a>(trace: &Trace<'a>, phase: &'a str, cancelled: bool) -> Vec<Station<'a>> {
    let mut stations: Vec<_> = trace
        .steps
        .iter()
        .filter_map(|step| Station::step(step, phase))
        .collect();
    for station in &mut stations {
        if (station.mark == Mark::Attention && !trace.waiting)
            || (!trace.active && station.mark.current())
        {
            station.mark = if cancelled {
                Mark::Cancelled
            } else {
                Mark::Reached
            };
        }
    }
    if trace.waiting
        && !stations
            .iter()
            .any(|station| station.mark == Mark::Attention)
    {
        stations.push(Station {
            id: Id::Review,
            label: "Review".into(),
            detail: phase,
            mark: Mark::Attention,
        });
    }
    stations.push(Station {
        id: Id::Answer,
        label: "Answer".into(),
        detail: if trace.answer.is_empty() {
            phase
        } else {
            trace.answer
        },
        mark: match trace.tone {
            Tone::Finished => Mark::Reached,
            Tone::Stopped if cancelled => Mark::Cancelled,
            Tone::Stopped => Mark::Failed,
            Tone::Active => Mark::Pending,
        },
    });
    stations
}

fn current(stations: &[Station<'_>]) -> usize {
    stations
        .iter()
        .rposition(|station| station.mark.current())
        .or_else(|| {
            stations
                .iter()
                .rposition(|station| station.mark != Mark::Pending)
        })
        .unwrap_or(0)
}

fn hidden_stations(width: f32, count: usize) -> usize {
    let capacity = ((width / STATION_WIDTH) as usize).max(2);
    if count > capacity {
        count - capacity + 1
    } else {
        0
    }
}

struct Track {
    marks: Vec<Mark>,
    slot: f32,
    current: usize,
    station: Id,
    run_started: Option<Instant>,
    moving: bool,
    active: bool,
}

impl Track {
    fn x(&self, index: usize) -> f32 {
        (index as f32 + 0.5) * self.slot
    }
}

#[derive(Default)]
struct Motion {
    key: Option<(Option<Instant>, Id)>,
    origin: f32,
    target: f32,
    changed: Option<Instant>,
    mounted: Option<Instant>,
}

impl Motion {
    fn position(&self, now: Instant) -> f32 {
        self.changed.map_or(self.target, |start| {
            let t = (now.saturating_duration_since(start).as_secs_f32() / TRAVEL.as_secs_f32())
                .min(1.0);
            self.origin + (self.target - self.origin) * (1.0 - (1.0 - t).powi(3))
        })
    }

    fn sync(&mut self, track: &Track, now: Instant) {
        let key = Some((track.run_started, track.station));
        let x = track.x(track.current);
        if self.key.is_none() || self.key.map(|(run, _)| run) != Some(track.run_started) {
            self.mounted = Some(now);
            self.changed = None;
        } else if !track.moving || (self.key == key && self.target != x) {
            self.changed = None;
        } else if self.key != key && self.target != x {
            self.origin = self.position(now);
            self.changed = Some(now);
        }
        self.target = x;
        self.key = key;
    }
}

impl canvas::Program<AgentMessage> for Track {
    type State = Motion;

    fn update(
        &self,
        state: &mut Motion,
        event: &Event,
        _: Rectangle,
        _: mouse::Cursor,
    ) -> Option<canvas::Action<AgentMessage>> {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return None;
        };
        state.sync(self, *now);
        self.moving
            .then(|| canvas::Action::request_redraw_at(*now + FRAME))
    }

    fn draw(
        &self,
        state: &Motion,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let colors = theme.palette();
        let rail = colors.text.scale_alpha(0.4);
        let now = Instant::now();
        let target = self.x(self.current);
        let light = if self.moving && state.key == Some((self.run_started, self.station)) {
            state.position(now)
        } else {
            target
        };
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let start = Point::new(self.x(0), RAIL_Y);
        for (end, color) in [
            (self.x(self.marks.len() - 1), rail),
            (target, colors.primary),
        ] {
            frame.stroke(
                &canvas::Path::line(start, Point::new(end, RAIL_Y)),
                canvas::Stroke::default().with_width(2.0).with_color(color),
            );
        }
        for (index, mark) in self.marks.iter().copied().enumerate() {
            let point = Point::new(self.x(index), RAIL_Y);
            let (stroke, fill) = match mark {
                Mark::Reached => (colors.primary, colors.primary),
                Mark::Active => (colors.primary, colors.background),
                Mark::Attention => (colors.warning, colors.background),
                Mark::Failed => (colors.danger, colors.danger),
                Mark::Cancelled | Mark::Pending => (rail, colors.background),
            };
            let pulsing = self.moving && index == self.current && mark.current();
            let alpha = if pulsing {
                let age = state
                    .mounted
                    .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32());
                (1.0 - (age * TAU / 1.4).cos()) * 0.5
            } else {
                0.0
            };
            if mark == Mark::Attention || pulsing {
                frame.fill(
                    &canvas::Path::circle(point, 8.0),
                    stroke.scale_alpha(0.24 + 0.22 * alpha),
                );
            }
            let dot = canvas::Path::circle(point, 5.4 + 1.3 * alpha);
            frame.fill(&dot, fill);
            frame.stroke(
                &dot,
                canvas::Stroke::default().with_width(1.6).with_color(stroke),
            );
        }
        if self.active {
            let point = Point::new(light, RAIL_Y);
            frame.fill(&canvas::Path::circle(point, 2.2), colors.text);
        }
        vec![frame.into_geometry()]
    }
}

fn station_label<'a>(station: &Station<'a>, width: f32, live: bool) -> Element<'a, AgentMessage> {
    let mark = station.mark;
    let label = text(station.label.clone())
        .size(10)
        .wrapping(text::Wrapping::None)
        .style(move |theme: &Theme| text::Style {
            color: match mark {
                Mark::Attention => Some(theme.palette().warning),
                Mark::Active => Some(theme.palette().primary),
                Mark::Failed => Some(theme.palette().danger),
                _ => None,
            },
        });
    let message = match station.id {
        Id::Step(id) => Some(AgentMessage::Design(Message::SelectStep(id))),
        Id::Review if live => Some(AgentMessage::Focus),
        _ => None,
    };
    let label = container(label)
        .center_x(width)
        .align_y(Alignment::End)
        .height(HEIGHT)
        .clip(true);
    let cell: Element<'a, AgentMessage> = if let Some(message) = message {
        button(label)
            .padding(0)
            .style(button::text)
            .on_press(message)
            .into()
    } else {
        label.into()
    };
    tooltip(
        cell,
        container(text(station.detail).size(12))
            .padding(5)
            .max_width(360),
        tooltip::Position::Top,
    )
    .into()
}

pub(super) fn view<'a>(
    activity: history::Activity<'a>,
    phase: &'a str,
    trace: &Trace<'a>,
    palette: Option<Palette>,
) -> Element<'a, AgentMessage> {
    let palette = Some(palette.unwrap_or_else(Palette::default_status));
    let cancelled = trace.run.map_or_else(
        || {
            activity
                .transcript
                .is_some_and(|trace| trace.state == State::Cancelled)
        },
        |run| run.phase == super::Phase::Cancelled,
    );
    let stations = Rc::new(stations(trace, phase, cancelled));
    let current = current(&stations);
    let (started, active, live, moving) = (
        trace.started_at,
        trace.active,
        trace.run.is_some(),
        trace.active && !trace.paused && !trace.reduced_motion,
    );
    let rail = move |size: iced::Size| {
        let hidden = hidden_stations(size.width, stations.len());
        let shown = &stations[hidden..];
        let count = shown.len() + usize::from(hidden > 0);
        let width = (size.width / count as f32).min(STATION_WIDTH * 1.5);
        let total_width = width * count as f32;
        let mut marks = Vec::with_capacity(count);
        let mut labels = row![].align_y(Alignment::End);
        if hidden > 0 {
            marks.push(Mark::Reached);
            let earlier = &stations[hidden - 1];
            labels = labels.push(station_label(
                &Station {
                    id: earlier.id,
                    label: format!("+{hidden}"),
                    detail: "Earlier stations",
                    mark: Mark::Reached,
                },
                width,
                live,
            ));
        }
        for station in shown {
            marks.push(station.mark);
            labels = labels.push(station_label(station, width, live));
        }
        let track = Track {
            marks,
            slot: width,
            current: current.saturating_sub(hidden) + usize::from(hidden > 0 && current >= hidden),
            station: stations[current].id,
            run_started: started,
            moving,
            active,
        };
        stack![canvas(track).width(total_width).height(HEIGHT), labels]
            .width(total_width)
            .height(HEIGHT)
            .into()
    };
    let goal = history::summary(activity.goal);
    let active_runs = trace.active_runs;
    let (paused, waiting, elapsed) = (trace.paused, trace.waiting, trace.elapsed_label());
    let hide = trace.run.map_or(
        AgentMessage::Design(Message::HideStatus),
        actions::hide_status,
    );
    let sample_tokens = (!live)
        .then(|| activity.transcript.and_then(|value| value.tokens))
        .flatten();
    responsive(move |size| {
        let title = metro_title(&goal, active, active_runs, (size.width * 0.29).min(170.0));
        let track = responsive(rail.clone()).height(HEIGHT);
        let actions = row![
            icon(
                if paused { "▶" } else { "Ⅱ" },
                if paused { "Resume" } else { "Pause" },
                (active && !waiting).then_some(AgentMessage::Pause)
            ),
            icon("■", "Cancel run", active.then_some(AgentMessage::Cancel)),
            log_table::status_icon_menu(activity, Status::Metro, |action| {
                icon("⋯", "More actions", Some(action))
            }),
            icon("✕", "Hide Agent status", Some(hide.clone())),
        ]
        .spacing(5)
        .align_y(Alignment::Center);
        let mut bar = row![
            if size.width >= 290.0 {
                Some(title)
            } else {
                None
            },
            track
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if size.width >= 450.0 {
            let usage = super::usage_label(activity, palette);
            if let Some(tokens) = sample_tokens.filter(|_| usage.is_none()) {
                let muted = palette.map(|palette| palette.muted.color());
                bar = bar.push(
                    text(format!("{} · {elapsed}", super::compact_tokens(tokens)))
                        .size(11)
                        .style(move |_| text::Style { color: muted }),
                );
            } else {
                bar = bar.push(usage).push(text(elapsed.clone()).size(11));
            }
        }
        if size.width < 220.0 {
            let scrollbar = scrollable::Scrollbar::new().width(2).scroller_width(2);
            bar = bar.push(
                container(
                    scrollable(actions).direction(scrollable::Direction::Horizontal(scrollbar)),
                )
                .max_width((size.width - 96.0).max(0.0)),
            );
        } else {
            bar = bar.push(actions);
        }
        palette::panel(
            palette,
            container(bar)
                .center_y(BAR_HEIGHT)
                .padding([0, 8])
                .clip(true),
        )
    })
    .height(BAR_HEIGHT)
    .into()
}

fn metro_title<'a>(
    goal: &str,
    active: bool,
    active_runs: usize,
    width: f32,
) -> Element<'a, AgentMessage> {
    let dot = container(space().width(7).height(7)).style(move |theme: &Theme| {
        container::Style::default()
            .background(if active {
                theme.palette().primary
            } else {
                Color::TRANSPARENT
            })
            .border(iced::border::rounded(4))
    });
    let mut title = row![
        dot,
        text("AI agent").size(13).font(Font {
            weight: font::Weight::Bold,
            ..Font::DEFAULT
        })
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if active_runs > 1 {
        title = title.push(text(format!("+{}", active_runs - 1)).size(11));
    }
    container(
        column![
            title,
            tooltip(
                text(goal.to_owned())
                    .size(11)
                    .wrapping(text::Wrapping::None),
                container(text(goal.to_owned()).size(12))
                    .padding(5)
                    .max_width(360),
                tooltip::Position::Top,
            ),
        ]
        .spacing(3),
    )
    .width(width)
    .clip(true)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_agent::design::inline::{Prompt, Transcript};

    #[test]
    fn metro_stations_follow_typed_steps_and_keep_the_answer_at_the_end() {
        let now = Instant::now();
        let mut transcript = Transcript {
            steps: vec![
                Step {
                    id: 1,
                    kind: Kind::Message,
                    ..Step::default()
                },
                Step {
                    id: 2,
                    kind: Kind::Action,
                    command: Some("sudo ls -la /tmp".into()),
                    ..Step::default()
                },
                Step {
                    id: 3,
                    kind: Kind::Thinking,
                    state: StepState::Running,
                    ..Step::default()
                },
            ],
            prompt: Some(Prompt::Approval {
                message: "Review".into(),
                commands: Vec::new(),
                allow_always: false,
                keyboard: false,
            }),
            ..Transcript::default()
        };
        let before = stations(
            &Trace::sample(&transcript, now, now, false),
            "Review",
            false,
        );
        assert_eq!(
            before.iter().map(|station| station.id).collect::<Vec<_>>(),
            [Id::Step(2), Id::Step(3), Id::Review, Id::Answer]
        );
        assert_eq!(before[0].label, "ls -la");
        assert_eq!(before[2].mark, Mark::Attention);
        assert_eq!(before[3].mark, Mark::Pending);
        assert_eq!(current(&before), 2);
        assert_eq!(hidden_stations(STATION_WIDTH * 2.0, before.len()), 3);
        assert_eq!(hidden_stations(STATION_WIDTH * 5.0, before.len()), 0);

        transcript.state = State::Cancelled;
        transcript.prompt = None;
        let after = stations(
            &Trace::sample(&transcript, now, now, false),
            "Cancelled",
            true,
        );
        assert_eq!(after[1].mark, Mark::Cancelled);
        assert_eq!(after.last().unwrap().mark, Mark::Cancelled);
        assert_eq!(current(&after), after.len() - 1);
    }

    #[test]
    fn metro_light_moves_to_a_new_station_and_snaps_with_reduced_motion() {
        let now = Instant::now();
        let mut track = Track {
            marks: vec![Mark::Active, Mark::Pending],
            slot: STATION_WIDTH,
            current: 0,
            station: Id::Step(1),
            run_started: Some(now),
            moving: true,
            active: true,
        };
        let mut motion = Motion::default();
        motion.sync(&track, now);
        let old = track.x(0);
        track.current = 1;
        track.station = Id::Step(2);
        motion.sync(&track, now);
        assert_eq!(motion.position(now), old);
        assert!((old..track.x(1)).contains(&motion.position(now + TRAVEL / 2)));
        assert_eq!(motion.position(now + TRAVEL), track.x(1));

        track.moving = false;
        track.current = 0;
        track.station = Id::Step(3);
        motion.sync(&track, now);
        assert_eq!(motion.position(now), old);
        assert!(motion.changed.is_none());
    }
}
