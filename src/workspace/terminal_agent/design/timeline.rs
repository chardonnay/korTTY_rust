//! The d02 rail follows typed events while keeping context menus on the exact step.

use std::time::{Duration, Instant, SystemTime};

use iced::widget::{Id, canvas, column, container, scrollable, stack, text};
use iced::{Color, Element, Font, Length, Padding, Point, Rectangle, Renderer, Theme, mouse};

use crate::terminal_agent::design::inline::{self, Kind, Role, Step, StepState, Transcript};

use super::{AgentMessage, Presentation, Run, command_review, fade, history::Activity, log_table};

const FADE: Duration = Duration::from_millis(220);
const SLIDE: Duration = Duration::from_millis(260);
const CONNECTOR: Duration = Duration::from_millis(320);
const RAIL_WIDTH: f32 = 18.0;
const CENTER_Y: f32 = 8.0;
const INBOUND_FRACTION: f32 = 0.18;

#[derive(Clone, Copy)]
struct Motion {
    now: Instant,
    wall: SystemTime,
    reduced: bool,
}

impl Motion {
    fn age(self, at: Option<Instant>) -> Option<Duration> {
        self.now.checked_duration_since(at?)
    }

    fn wall_age(self, at: Option<SystemTime>) -> Option<Duration> {
        self.wall.duration_since(at?).ok()
    }

    fn step_age(self, presentation: &Presentation, step: &Step) -> Option<Duration> {
        self.age(
            presentation
                .log_table
                .seen_at(log_table::Row::Step(step.id)),
        )
        .or_else(|| self.age(step.started_at))
        .or_else(|| self.wall_age(step.recorded_at))
    }

    fn fraction(self, age: Option<Duration>, duration: Duration) -> f32 {
        age.filter(|_| !self.reduced).map_or(1.0, |age| {
            (age.as_secs_f32() / duration.as_secs_f32()).min(1.0)
        })
    }

    fn connector(self, age: Option<Duration>) -> f32 {
        1.0 - (1.0 - self.fraction(age, CONNECTOR)).powi(3)
    }

    fn pulse(self, age: Option<Duration>, enabled: bool) -> f32 {
        age.filter(|_| enabled && !self.reduced).map_or(1.0, |age| {
            1.0 + 0.35 * (0.5 - 0.5 * (age.as_secs_f32() * std::f32::consts::TAU / 1.3).cos())
        })
    }
}

pub(super) fn refresh_interval(
    trace: &Transcript,
    presentation: &Presentation,
    now: Instant,
    running: bool,
    reduced_motion: bool,
) -> Option<u64> {
    let motion = Motion {
        now,
        wall: SystemTime::now(),
        reduced: reduced_motion,
    };
    let review = motion.age(presentation.review_started);
    let running_step = running
        && trace
            .steps
            .iter()
            .any(|step| step.state == StepState::Running);
    if !reduced_motion
        && (running_step
            || review.is_some_and(|age| running || age < CONNECTOR)
            || motion
                .wall_age(trace.final_answer_at)
                .into_iter()
                .chain(
                    trace
                        .steps
                        .iter()
                        .filter_map(|step| motion.step_age(presentation, step)),
                )
                .any(|age| age < CONNECTOR))
    {
        Some(16)
    } else {
        running_step.then_some(1_000)
    }
}

pub(super) fn view<'a>(
    activity: Activity<'a>,
    trace: &'a Transcript,
    reduced_motion: bool,
) -> Element<'a, AgentMessage> {
    let motion = Motion {
        now: Instant::now(),
        wall: SystemTime::now(),
        reduced: reduced_motion || activity.live_run.is_some_and(|run| run.reopened_history),
    };
    let size = activity.activity_font;
    let active = activity
        .live_run
        .map_or_else(|| !trace.state.finished(), Run::active);
    let pulsing = activity.live_run.map_or_else(
        || trace.state == inline::State::Running,
        |run| active && !run.paused && !run.cancel.requested(),
    );
    let review = command_review::card(activity);
    let answer =
        (!active && !trace.final_answer.trim().is_empty()).then(|| trace.final_answer.trim());
    let review_age = motion.age(activity.presentation.review_started);
    let answer_age = motion
        .wall_age(trace.final_answer_at)
        .or_else(|| motion.age(activity.presentation.finish_time()));
    let tail_age = if review.is_some() {
        review_age
    } else {
        answer_age
    };
    let has_tail = review.is_some() || answer.is_some();
    let mut items = column![].width(Length::Fill);
    for (index, step) in trace.steps.iter().enumerate() {
        let age = motion.step_age(activity.presentation, step);
        let next = trace
            .steps
            .get(index + 1)
            .map(|next| motion.step_age(activity.presentation, next))
            .or_else(|| has_tail.then_some(tail_age))
            .map(|age| motion.connector(age));
        let command = step.kind == Kind::Action;
        let title = inline::clean(step.command.as_deref().unwrap_or(&step.title), false);
        let label = text(title).size(size).font(if command {
            Font::MONOSPACE
        } else {
            Font::DEFAULT
        });
        let body = column![
            label,
            metadata(step, motion.now).map(|value| text(value).size(size - 1.0))
        ]
        .spacing(3)
        .width(Length::Fill);
        let role = if command && step.state == StepState::Completed {
            Role::Accent
        } else {
            inline::marker(step, "◦").role
        };
        let node = entry(
            body.into(),
            motion,
            Node {
                role,
                age,
                pulse: pulsing && step.state == StepState::Running,
                incoming: index > 0,
                next,
                review: false,
            },
        );
        items = items.push(if let Some(run) = activity.live_run {
            log_table::step_context(node, run, step, activity.surface.activity_entry(index))
        } else {
            node
        });
    }
    let end = if let Some(card) = review {
        items = items.push(entry(
            card,
            motion,
            Node {
                role: Role::Warn,
                age: review_age,
                pulse: pulsing,
                incoming: !trace.steps.is_empty(),
                next: None,
                review: true,
            },
        ));
        activity
            .live_run
            .map_or(0, |run| run.epoch)
            .saturating_mul(3)
            .saturating_add(1)
    } else {
        if let Some(answer) = answer {
            items = items.push(entry(
                text(answer).size(size).into(),
                motion,
                Node {
                    role: Role::Ok,
                    age: answer_age,
                    pulse: false,
                    incoming: !trace.steps.is_empty(),
                    next: None,
                    review: false,
                },
            ));
        }
        trace
            .steps
            .last()
            .map_or(0, |step| step.id)
            .saturating_mul(3)
            .saturating_add(if answer.is_some() { 2 } else { 0 })
    };
    scrollable(items)
        .id(activity
            .presentation
            .timeline_viewport
            .get_or_init(Id::unique)
            .clone())
        .width(Length::Fill)
        .height(Length::Fill)
        .follow_bottom(true, end)
        .into()
}

pub(super) fn metadata(step: &Step, now: Instant) -> Option<String> {
    let elapsed = step
        .elapsed_at(now)
        .map(|millis| format!("{:.1} s", millis as f64 / 1_000.0));
    let tokens = step.tokens.map(|tokens| format!("{tokens} tokens"));
    let result = step
        .exit_code
        .map(|code| format!("Exit {code}"))
        .or_else(|| {
            (step.kind == Kind::Action || step.state != StepState::Completed)
                .then(|| log_table::state_label(step).into())
        });
    let metadata = [result, elapsed, tokens]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    (!metadata.is_empty()).then_some(metadata)
}

struct Node {
    role: Role,
    age: Option<Duration>,
    pulse: bool,
    incoming: bool,
    next: Option<f32>,
    review: bool,
}

fn entry<'a>(
    body: Element<'a, AgentMessage>,
    motion: Motion,
    node: Node,
) -> Element<'a, AgentMessage> {
    // Review controls must be visible as soon as they can accept terminal keys.
    let body_age = node.age.filter(|_| !node.review);
    let slide = 12.0 * (1.0 - motion.fraction(body_age, SLIDE)).powi(3);
    let body = container(fade::view(body, motion.fraction(body_age, FADE)))
        .width(Length::Fill)
        .padding(Padding {
            left: RAIL_WIDTH + 6.0 + slide,
            right: 3.0,
            top: 0.0,
            bottom: 7.0,
        });
    let incoming = if node.incoming {
        ((motion.connector(node.age) - (1.0 - INBOUND_FRACTION)) / INBOUND_FRACTION).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let outgoing = (node.next.unwrap_or(0.0) / (1.0 - INBOUND_FRACTION)).min(1.0);
    stack![body]
        .push_under(
            canvas(Rail {
                role: node.role,
                opacity: motion.fraction(node.age, FADE),
                pulse: motion.pulse(node.age, node.pulse),
                incoming,
                outgoing,
            })
            .width(RAIL_WIDTH)
            .height(Length::Fill),
        )
        .into()
}

struct Rail {
    role: Role,
    opacity: f32,
    pulse: f32,
    incoming: f32,
    outgoing: f32,
}

impl canvas::Program<AgentMessage> for Rail {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();
        let ink = match self.role {
            Role::Ok => palette.success.base.color,
            Role::Warn => palette.warning.base.color,
            Role::Error => palette.danger.base.color,
            Role::Accent | Role::Think => palette.primary.base.color,
            _ => palette.background.weak.text.scale_alpha(0.7),
        };
        let center = Point::new(bounds.width / 2.0, CENTER_Y.min(bounds.height / 2.0));
        let connector = palette.background.strong.color;
        line(
            &mut frame,
            center.x,
            0.0,
            center.y * self.incoming,
            connector,
        );
        line(
            &mut frame,
            center.x,
            center.y,
            center.y + (bounds.height - center.y) * self.outgoing,
            connector,
        );
        let radius = (4.4 * self.pulse).min(center.y);
        frame.fill(
            &canvas::Path::circle(center, radius),
            ink.scale_alpha(self.opacity),
        );
        vec![frame.into_geometry()]
    }
}

fn line(frame: &mut canvas::Frame, x: f32, start: f32, end: f32, color: Color) {
    if end > start {
        frame.stroke(
            &canvas::Path::line(Point::new(x, start), Point::new(x, end)),
            canvas::Stroke::default().with_color(color).with_width(1.6),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_motion_grows_connectors_and_respects_reduced_animations() {
        let animated = Motion {
            now: Instant::now(),
            wall: SystemTime::now(),
            reduced: false,
        };
        assert_eq!(animated.connector(Some(Duration::ZERO)), 0.0);
        assert!(animated.connector(Some(CONNECTOR / 2)) > 0.0);
        assert_eq!(animated.connector(Some(CONNECTOR)), 1.0);
        assert!(animated.pulse(Some(Duration::from_millis(650)), true) > 1.2);
        assert_eq!(animated.pulse(Some(Duration::from_millis(650)), false), 1.0);

        let still = Motion {
            reduced: true,
            ..animated
        };
        assert_eq!(still.connector(Some(Duration::ZERO)), 1.0);
        assert_eq!(still.fraction(Some(Duration::ZERO), FADE), 1.0);
        assert_eq!(still.pulse(Some(Duration::from_millis(650)), true), 1.0);
    }
}
