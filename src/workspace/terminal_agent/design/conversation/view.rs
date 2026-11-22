use std::time::{Duration, Instant, SystemTime};

use iced::widget::{button, canvas, column, container, scrollable, text};
use iced::{Element, Font, Length, Point, Rectangle, Renderer, Theme, mouse};

use crate::terminal_agent::design::inline::{
    Kind, State as TranscriptState, Step, StepState, Transcript,
};

use super::{Activity, Message, OutputKey, prompt};
use crate::workspace::terminal_agent::design::{self, Palette, history, log_table};
use crate::workspace::terminal_agent::spotlight::scale_card;

const ENTRANCE: Duration = Duration::from_millis(280);

#[derive(Clone, Copy)]
struct Entrance {
    now: Instant,
    wall: SystemTime,
    reduced: bool,
}

impl Entrance {
    fn step_age(self, step: &Step) -> Option<Duration> {
        self.age(step.started_at)
            .or_else(|| self.wall_age(step.recorded_at))
    }

    fn age(self, at: Option<Instant>) -> Option<Duration> {
        self.now.checked_duration_since(at?)
    }

    fn wall_age(self, at: Option<SystemTime>) -> Option<Duration> {
        self.wall.duration_since(at?).ok()
    }

    fn apply<'a>(
        self,
        bubble: Element<'a, Message>,
        age: Option<Duration>,
    ) -> Element<'a, Message> {
        scale_card(bubble, entrance_scale(age, self.reduced))
    }
}

pub(super) fn entrance_scale(age: Option<Duration>, reduced: bool) -> f32 {
    let Some(age) = age.filter(|age| !reduced && *age < ENTRANCE) else {
        return 1.0;
    };
    let progress = age.as_secs_f32() / ENTRANCE.as_secs_f32();
    1.0 - 0.18 * (1.0 - progress).powi(2) * (std::f32::consts::TAU * progress).cos()
}

pub(in crate::workspace::terminal_agent::design) fn entrance_interval(
    trace: &Transcript,
    started: Option<Instant>,
    review_started: Option<Instant>,
    now: Instant,
    reduced: bool,
) -> Option<u64> {
    if reduced {
        return None;
    }
    let motion = Entrance {
        now,
        wall: SystemTime::now(),
        reduced,
    };
    [
        motion.age(started),
        motion.age(review_started),
        motion.wall_age(trace.final_answer_at),
    ]
    .into_iter()
    .chain(trace.steps.iter().map(|step| motion.step_age(step)))
    .flatten()
    .any(|age| age < ENTRANCE)
    .then_some(16)
}

pub(in crate::workspace::terminal_agent::design) fn refresh_interval(
    trace: &Transcript,
    pending: bool,
    reduced_motion: bool,
) -> Option<u64> {
    if !pending {
        return None;
    }
    if !reduced_motion {
        return Some(33);
    }
    trace
        .steps
        .iter()
        .any(|step| {
            step.command.is_some() && step.state == StepState::Running && step.started_at.is_some()
        })
        .then_some(1000)
}

pub(in crate::workspace::terminal_agent::design) fn thread<'a>(
    activity: Activity<'a>,
    trace: &'a Transcript,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'a, Message> {
    let now = Instant::now();
    let motion = Entrance {
        now,
        wall: SystemTime::now(),
        reduced: reduced_motion || activity.live_run.is_some_and(|run| run.reopened_history),
    };
    let earlier = activity
        .owner
        .zip(activity.live_run)
        .into_iter()
        .flat_map(|((state, _), run)| state.earlier_conversation(run));
    let order = activity.live_run.map_or(0, |run| run.started_order);
    let mut messages = column![].spacing(10).width(Length::Fill);
    for turn in earlier {
        messages = messages.push(turn_view(
            activity,
            turn.order(),
            turn.transcript(),
            false,
            palette,
            Entrance {
                reduced: true,
                ..motion
            },
        ));
    }
    messages = messages.push(turn_view(activity, order, trace, true, palette, motion));
    if let Some(card) = design::command_review::card(activity) {
        messages =
            messages.push(motion.apply(card, motion.age(activity.presentation.review_started)));
    }
    let pending = activity.live_run.map_or_else(
        || trace.state == TranscriptState::Running && trace.prompt.is_none(),
        super::super::super::Run::conversation_pending,
    );
    if pending {
        let elapsed = now.saturating_duration_since(activity.started.unwrap_or(now));
        messages = messages.push(
            container(
                canvas(Dots(dot_lift(elapsed, reduced_motion)))
                    .width(34)
                    .height(15),
            )
            .padding([2, 5])
            .style(container::bordered_box),
        );
    }
    column![
        scrollable(messages).anchor_bottom().height(Length::Fill),
        prompt(activity)
    ]
    .spacing(6)
    .height(Length::Fill)
    .into()
}

fn turn_view<'a>(
    activity: Activity<'a>,
    turn: u64,
    trace: &'a Transcript,
    current: bool,
    palette: Option<Palette>,
    motion: Entrance,
) -> Element<'a, Message> {
    let size = activity.activity_font;
    let question = if trace.goal.is_empty() && current {
        activity.goal
    } else {
        &trace.goal
    };
    let question = motion.apply(
        container(text(question).size(size))
            .padding(8)
            .style(container::rounded_box)
            .into(),
        motion.age(activity.started),
    );
    let mut bubbles = column![
        container(question)
            .width(Length::Fill)
            .align_right(Length::Fill)
    ]
    .spacing(6)
    .width(Length::Fill);
    for step in &trace.steps {
        let key = OutputKey {
            panel: activity.live_run.map_or(0, |run| run.started_order),
            turn,
            step: step.id,
        };
        let bubble = if let Some(command) = step
            .command
            .as_deref()
            .filter(|command| !command.trim().is_empty())
        {
            command_bubble(step, command, key, activity, palette, motion.now)
        } else {
            if step.kind == Kind::Thinking && step.state == StepState::Running {
                continue;
            }
            let title = step.title.trim();
            let detail = step.detail.trim();
            if title.is_empty() && detail.is_empty() {
                continue;
            }
            let color = palette
                .filter(|_| step.kind == Kind::Error)
                .map(|palette| palette.error.color());
            container(
                column![
                    (!title.is_empty())
                        .then(|| text(title).size(size).style(move |_| text::Style { color })),
                    (!detail.is_empty() && detail != title).then(|| text(detail).size(size)),
                ]
                .spacing(3),
            )
            .padding(7)
            .style(container::bordered_box)
            .into()
        };
        let bubble = motion.apply(bubble, motion.step_age(step));
        let bubble = match activity.live_run {
            Some(run) if current => log_table::step_context(
                bubble,
                run,
                step,
                activity.surface.activity_entry(step.id as usize),
            ),
            Some(run) => match activity.surface.earlier_conversation(turn) {
                Some(surface) => log_table::earlier_step_context(bubble, run, step, surface, key),
                None => bubble,
            },
            None => bubble,
        };
        bubbles = bubbles.push(bubble);
    }
    if current && activity.live_run.is_some() {
        let selected = activity.presentation.selection(activity.activity.len());
        for (index, entry) in activity.activity.iter().enumerate() {
            if activity.bound_step(index).is_some()
                || entry.trim().is_empty()
                || entry.trim() == trace.final_answer.trim()
            {
                continue;
            }
            bubbles = bubbles.push(
                container(
                    column![
                        button(text(history::summary(entry)).size(size))
                            .on_press(history::select(activity, index))
                            .style(button::text),
                        (index == selected || activity.presentation.activity.expand_all)
                            .then(|| text(entry).size(size)),
                    ]
                    .spacing(3),
                )
                .padding(7)
                .style(container::bordered_box),
            );
        }
    }
    if !trace.final_answer.trim().is_empty() {
        bubbles = bubbles.push(
            motion.apply(
                container(text(trace.final_answer.trim()).size(size))
                    .padding(8)
                    .style(container::bordered_box)
                    .into(),
                motion.wall_age(trace.final_answer_at),
            ),
        );
    }
    bubbles.into()
}

fn command_bubble<'a>(
    step: &'a Step,
    command: &'a str,
    key: OutputKey,
    activity: Activity<'a>,
    palette: Option<Palette>,
    now: Instant,
) -> Element<'a, Message> {
    let size = activity.activity_font;
    let accent = palette.map(|palette| palette.accent.color());
    let muted = palette.map(|palette| palette.muted.color());
    let expand_all = activity.presentation.activity.expand_all;
    let opened = expand_all || activity.presentation.conversation_output.contains(&key);
    let toggleable = !step.output.is_empty() && !expand_all;
    let mut result = step.exit_code.map_or_else(
        || log_table::state_label(step).to_owned(),
        |code| format!("Exit {code}"),
    );
    if step.state == StepState::Running
        && let Some(elapsed) = step.elapsed_at(now)
    {
        result.push_str(&format!(" · {} s", elapsed / 1000));
    }
    let heading = column![
        text(command)
            .font(Font::MONOSPACE)
            .size(size)
            .wrapping(text::Wrapping::WordOrGlyph)
            .style(move |_| text::Style { color: accent }),
        toggleable.then(|| text(if opened {
            "▾ Hide output"
        } else {
            "▸ Show output"
        })
        .size(size - 1.0)),
    ]
    .spacing(3);
    let heading: Element<'a, Message> = if !toggleable {
        heading.into()
    } else {
        button(heading)
            .padding(0)
            .style(button::text)
            .on_press(Message::Design(design::Message::ConversationOutput(key)))
            .into()
    };
    let mut content = column![
        heading,
        text(result)
            .size(size - 1.0)
            .style(move |_| text::Style { color: muted })
    ]
    .spacing(3);
    if opened {
        content = content.extend(step.output.iter().map(|line| {
            text(line)
                .font(Font::MONOSPACE)
                .size(size - 1.0)
                .wrapping(text::Wrapping::WordOrGlyph)
                .style(move |_| text::Style { color: muted })
                .into()
        }));
    }
    container(content)
        .padding(7)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}

struct Dots([f32; 3]);

pub(super) fn dot_lift(elapsed: Duration, reduced_motion: bool) -> [f32; 3] {
    if reduced_motion {
        return [0.0; 3];
    }
    std::array::from_fn(|index| {
        let offset = elapsed.as_millis().saturating_sub(120 * index as u128) % 640;
        let angle = offset as f32 / 640.0 * std::f32::consts::TAU;
        2.0 * (1.0 - angle.cos())
    })
}

impl canvas::Program<Message> for Dots {
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
        let color = theme.palette().text.scale_alpha(0.7);
        for (index, lift) in self.0.iter().enumerate() {
            let center = Point::new(6.0 + index as f32 * 10.0, 10.0 - lift);
            frame.fill(&canvas::Path::circle(center, 2.5), color);
        }
        vec![frame.into_geometry()]
    }
}
