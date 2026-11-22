//! The d06 plan and execution columns share the run's typed, masked history.

use std::time::{Duration, Instant};

use iced::widget::{button, canvas, column, progress_bar, row, scrollable, text};
use iced::{Alignment, Element, Font, Length, Point, Rectangle, Renderer, Theme, mouse};

use crate::terminal_agent::design::inline::{self, Kind, Role, Step, StepState, Transcript};

use super::{
    AgentMessage, Presentation, Run, command_review,
    history::{self, Activity},
    log_table::{self, Row},
    status,
};

const OUTPUT_LINES: usize = 5;
const PROGRESS_DURATION: Duration = Duration::from_millis(360);
const CHECK_DURATION: Duration = Duration::from_millis(320);

#[derive(Default)]
pub(super) struct Progress {
    target: f32,
    transition: Option<(f32, Instant)>,
    checks: Vec<(u64, Instant)>,
}

impl Progress {
    pub(super) fn sync(&mut self, transcript: &Transcript, finished: bool, now: Instant) {
        let target = if finished {
            1.0
        } else if transcript.steps.is_empty() {
            0.05
        } else {
            let done = transcript
                .steps
                .iter()
                .filter(|step| step.state != StepState::Running)
                .count();
            done as f32 / (transcript.steps.len() + 1) as f32
        };
        if target != self.target {
            self.transition = Some((self.value(now, false), now));
            self.target = target;
        }
        self.checks
            .retain(|(id, _)| transcript.steps.iter().any(|step| step.id == *id));
        for step in transcript.steps.iter().filter(|step| has_check(step)) {
            if !self.checks.iter().any(|(id, _)| *id == step.id) {
                self.checks.push((step.id, now));
            }
        }
    }

    fn value(&self, now: Instant, reduced_motion: bool) -> f32 {
        if let Some((from, started)) = self.transition.filter(|_| !reduced_motion) {
            let t = (now.saturating_duration_since(started).as_secs_f32()
                / PROGRESS_DURATION.as_secs_f32())
            .min(1.0);
            return from + (self.target - from) * t * t * (3.0 - 2.0 * t);
        }
        self.target
    }

    fn check(&self, id: u64, now: Instant, reduced_motion: bool) -> f32 {
        let started = self.checks.iter().find(|(step, _)| *step == id);
        started
            .filter(|_| !reduced_motion)
            .map_or(1.0, |(_, start)| {
                let progress = now.saturating_duration_since(*start).min(CHECK_DURATION);
                let t = progress.as_secs_f32() / CHECK_DURATION.as_secs_f32();
                1.0 - (1.0 - t) * (1.0 - t)
            })
    }

    fn animating(&self, transcript: &Transcript, now: Instant) -> bool {
        self.transition
            .is_some_and(|(_, start)| now.saturating_duration_since(start) < PROGRESS_DURATION)
            || transcript
                .steps
                .iter()
                .any(|step| has_check(step) && self.check(step.id, now, false) < 1.0)
    }
}

fn has_check(step: &Step) -> bool {
    step.state == StepState::Completed && matches!(step.kind, Kind::Action | Kind::Question)
}

pub(super) fn refresh_interval(
    transcript: &Transcript,
    presentation: &Presentation,
    now: Instant,
    active: bool,
    spinning: bool,
    reduced_motion: bool,
) -> Option<u64> {
    if !reduced_motion && presentation.plan_progress.animating(transcript, now) {
        Some(16)
    } else if active
        && transcript
            .steps
            .iter()
            .any(|step| step.state == StepState::Running)
    {
        Some(if spinning && !reduced_motion {
            120
        } else {
            1_000
        })
    } else {
        None
    }
}

pub(super) fn view(activity: Activity<'_>, reduced_motion: bool) -> Element<'_, AgentMessage> {
    let now = Instant::now();
    let size = activity.activity_font;
    let trace = activity.transcript;
    let restored = activity.live_run.is_some_and(|run| run.reopened_history);
    let active = activity.live_run.map_or_else(
        || trace.is_some_and(|trace| !trace.state.finished()),
        Run::active,
    );
    let spinning = !reduced_motion
        && activity.live_run.map_or_else(
            || trace.is_some_and(|trace| trace.state == inline::State::Running),
            |run| active && !run.paused && !run.cancel.requested(),
        );
    let waiting = activity.live_run.map_or_else(
        || trace.is_some_and(|trace| trace.prompt.is_some()),
        Run::awaiting_input,
    );
    let goal = trace.map_or(activity.goal, |trace| &trace.goal);
    let mut steps = column![text("Goal").size(size + 1.0), text(goal).size(size)].spacing(5);
    if let Some(plan) = activity.plan {
        steps = steps.push(text(plan).size(activity.plan_font));
    }
    steps = steps.push(text("Activity").size(size + 1.0));
    let selected = activity.presentation.selection(activity.activity.len());
    if let Some(trace) = trace.filter(|trace| !trace.steps.is_empty()) {
        for (index, step) in trace.steps.iter().enumerate() {
            let symbol = inline::marker(step, spinner(activity, step, now, spinning));
            let mark: Element<'_, AgentMessage> = if has_check(step) {
                let fraction = activity.presentation.plan_progress.check(
                    step.id,
                    now,
                    reduced_motion || restored,
                );
                canvas(CheckMark(fraction)).width(16).height(16).into()
            } else {
                text(symbol.text)
                    .width(16)
                    .size(size)
                    .style(move |theme| marker_style(theme, symbol.role))
                    .into()
            };
            let title = step.command.as_deref().unwrap_or(&step.title);
            let label = format!(
                "{}  {}",
                index + 1,
                history::summary(&inline::clean(title, false))
            );
            let item = row![mark, text(label).size(size - 1.0)]
                .align_y(Alignment::Center)
                .spacing(5);
            let surface = activity.surface.activity_entry(index);
            steps = steps.push(step_menu(activity, step, surface, item.into()));
        }
    } else {
        for (index, entry) in activity.activity.iter().enumerate() {
            steps = steps.push(
                activity.entry_menu(
                    index,
                    button(
                        text(format!("{}  {}", index + 1, history::summary(entry)))
                            .size(size - 1.0),
                    )
                    .on_press(history::select(activity, index))
                    .style(if index == selected {
                        button::primary
                    } else {
                        button::text
                    }),
                ),
            );
        }
    }
    if active && !waiting {
        steps = steps.push(text("○  Next step").size(size - 1.0));
    }

    let execution = command_review::card(activity).unwrap_or_else(|| {
        if !active
            && let Some(answer) = trace
                .map(|trace| trace.final_answer.trim())
                .filter(|answer| !answer.is_empty())
        {
            return text(format!("◆ {answer}")).size(size).into();
        }
        if let Some(step) = trace.and_then(|trace| {
            trace
                .steps
                .iter()
                .rev()
                .find(|step| step.kind == Kind::Action)
        }) {
            return command(activity, step, now, spinning);
        }
        if trace.is_none_or(|trace| trace.steps.is_empty()) && !activity.activity.is_empty() {
            return history::detail(activity, selected);
        }
        text(
            activity
                .live_run
                .map_or("Waiting for a command", |run| status::phase(run).0),
        )
        .size(size)
        .into()
    });
    let progress = if restored {
        1.0
    } else {
        activity
            .presentation
            .plan_progress
            .value(now, reduced_motion)
    };
    column![
        progress_bar(0.0..=1.0, progress).girth(3),
        row![
            scrollable(steps)
                .height(Length::Fill)
                .width(Length::FillPortion(2)),
            scrollable(execution)
                .height(Length::Fill)
                .width(Length::FillPortion(3)),
        ]
        .spacing(10),
    ]
    .spacing(4)
    .height(Length::Fill)
    .into()
}

struct CheckMark(f32);

fn check_points(fraction: f32) -> [Point; 3] {
    let start = Point::new(2.0, 7.0);
    let bend = Point::new(6.0, 11.0);
    let end = Point::new(13.0, 3.0);
    if fraction >= 1.0 {
        return [start, bend, end];
    }
    // The stroke advances by distance, with the same speed across both segments.
    let first = 4.0_f32.hypot(4.0);
    let second = 7.0_f32.hypot(8.0);
    let length = fraction * (first + second);
    let middle = start + (bend - start) * (length / first).min(1.0);
    let tip = middle + (end - bend) * ((length - first) / second).clamp(0.0, 1.0);
    [start, middle, tip]
}

impl canvas::Program<AgentMessage> for CheckMark {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if self.0 <= 0.0 {
            return Vec::new();
        }
        let [start, middle, tip] = check_points(self.0);
        let check = canvas::Path::new(|path| {
            path.move_to(start);
            path.line_to(middle);
            if tip != middle {
                path.line_to(tip);
            }
        });
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.stroke(
            &check,
            canvas::Stroke::default()
                .with_color(theme.extended_palette().success.base.color)
                .with_width(2.0)
                .with_line_cap(canvas::LineCap::Round)
                .with_line_join(canvas::LineJoin::Round),
        );
        vec![frame.into_geometry()]
    }
}

fn step_menu<'a>(
    activity: Activity<'a>,
    step: &Step,
    surface: log_table::Surface,
    item: Element<'a, AgentMessage>,
) -> Element<'a, AgentMessage> {
    if let Some(run) = activity.live_run {
        log_table::step_context(item, run, step, surface)
    } else {
        item
    }
}

fn command<'a>(
    activity: Activity<'a>,
    step: &Step,
    now: Instant,
    spinning: bool,
) -> Element<'a, AgentMessage> {
    let size = activity.activity_font;
    let label = inline::clean(step.command.as_deref().unwrap_or(&step.title), true);
    let mut state = if step.state == StepState::Running {
        format!("{} Running", spinner(activity, step, now, spinning))
    } else {
        step.exit_code.map_or_else(
            || log_table::state_label(step).into(),
            |code| format!("Exit {code}"),
        )
    };
    if let Some(elapsed) = step.elapsed_at(now) {
        state.push_str(&format!(" · {:.1} s", elapsed as f64 / 1000.0));
    }
    let role = inline::marker(step, "◦").role;
    let mut body = column![
        text(format!("$ {label}"))
            .font(Font::MONOSPACE)
            .size(size)
            .style(|theme| marker_style(theme, Role::Accent)),
        text(state)
            .font(Font::MONOSPACE)
            .size(size - 1.0)
            .style(move |theme| marker_style(theme, role)),
    ]
    .spacing(4)
    .width(Length::Fill);
    for line in &step.output[step.output.len().saturating_sub(OUTPUT_LINES)..] {
        body = body.push(
            text(inline::clean(line, false))
                .font(Font::MONOSPACE)
                .size(size - 1.0),
        );
    }
    step_menu(activity, step, activity.surface.execution(), body.into())
}

fn spinner(activity: Activity<'_>, step: &Step, now: Instant, spinning: bool) -> &'static str {
    if spinning {
        activity
            .presentation
            .log_table
            .spinner(Row::Step(step.id), now)
    } else {
        "◦"
    }
}

pub(super) fn marker_style(theme: &Theme, role: Role) -> text::Style {
    let palette = theme.extended_palette();
    let color = match role {
        Role::Ok => palette.success.base.color,
        Role::Error => palette.danger.base.color,
        Role::Warn => palette.warning.base.color,
        Role::Accent | Role::Think => palette.primary.base.color,
        _ => palette.background.weak.text,
    };
    text::Style { color: Some(color) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_progress_eases_from_current_value_and_snaps_with_reduced_motion() {
        let now = Instant::now();
        let mut progress = Progress::default();
        let transcript = Transcript {
            steps: vec![
                Step::default(),
                Step {
                    state: StepState::Running,
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        progress.sync(&transcript, false, now);
        let halfway = now + PROGRESS_DURATION / 2;
        assert_eq!(progress.value(now, true), 1.0 / 3.0);
        assert_eq!(progress.value(halfway, false), 1.0 / 6.0);

        progress.sync(&transcript, true, halfway);
        assert_eq!(progress.value(halfway, false), 1.0 / 6.0);
        assert_eq!(progress.value(halfway, true), 1.0);
        assert_eq!(progress.value(halfway + PROGRESS_DURATION, false), 1.0);
    }

    #[test]
    fn successful_step_draws_its_check_once_and_reduced_motion_shows_it_immediately() {
        let start = Instant::now();
        let mut presentation = Presentation::default();
        let mut trace = Transcript {
            steps: vec![
                Step {
                    id: 1,
                    kind: Kind::Action,
                    state: StepState::Completed,
                    ..Step::default()
                },
                Step {
                    id: 2,
                    kind: Kind::Question,
                    state: StepState::Failed,
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        presentation.plan_progress.sync(&trace, false, start);
        let completed = start + PROGRESS_DURATION;
        trace.steps[1].state = StepState::Completed;
        presentation.plan_progress.sync(&trace, false, completed);
        assert_eq!(presentation.plan_progress.check(1, completed, false), 1.0);
        assert_eq!(presentation.plan_progress.check(2, completed, false), 0.0);
        assert_eq!(presentation.plan_progress.check(2, completed, true), 1.0);
        assert_eq!(
            refresh_interval(&trace, &presentation, completed, true, true, false),
            Some(16)
        );
        assert_eq!(
            refresh_interval(&trace, &presentation, completed, true, true, true),
            None
        );

        let halfway = completed + CHECK_DURATION / 2;
        trace.steps[1].detail = "Updated answer".into();
        presentation.plan_progress.sync(&trace, false, halfway);
        assert_eq!(presentation.plan_progress.check(2, halfway, false), 0.75);
        assert_eq!(
            refresh_interval(
                &trace,
                &presentation,
                completed + CHECK_DURATION,
                true,
                true,
                false
            ),
            None
        );
        trace.steps.remove(0);
        presentation.plan_progress.sync(&trace, false, halfway);
        assert_eq!(presentation.plan_progress.checks.len(), 1);
    }

    #[test]
    fn check_stroke_draws_the_short_segment_before_the_long_one() {
        let [start, bend, end] = check_points(1.0);
        assert_eq!(check_points(0.0), [start; 3]);
        let [_, middle, tip] = check_points(0.2);
        assert!(start.x < middle.x && middle.x < bend.x);
        assert_eq!(tip, middle);
        let [_, middle, tip] = check_points(0.75);
        assert_eq!(middle, bend);
        assert!(bend.x < tip.x && tip.x < end.x);
        assert!(end.y < tip.y && tip.y < bend.y);
    }
}
