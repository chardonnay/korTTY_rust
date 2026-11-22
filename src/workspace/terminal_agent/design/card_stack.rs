//! The d07 stack keeps typed receipts and the run's answer in browsing order.

use std::time::{Duration, Instant, SystemTime};

use iced::widget::{button, column, container, mouse_area, row, scrollable, stack, text};
use iced::{Element, Font, Length, Padding, mouse};

use crate::terminal_agent::design::inline::{self, Kind, Role, Step, StepState, Transcript};
use crate::workspace::terminal_agent::spotlight::scale_card;

use super::{
    AgentMessage, Message, Presentation, Run, command_review, fade,
    history::{self, Activity},
    log_table::{self, Row},
    plan_execution::marker_style,
    timeline::metadata,
};

const BEHIND: usize = 2;
const STEP_BACK: f32 = 21.0;
const OUTPUT_LINES: usize = 2;
const RISE: Duration = Duration::from_millis(380);
const FADE: Duration = Duration::from_millis(200);
const RECEDE: Duration = Duration::from_millis(300);

pub(super) fn entry_count(trace: Option<&Transcript>, legacy: usize, active: bool) -> usize {
    typed(trace).map_or(legacy, |trace| trace.steps.len())
        + usize::from(answer(trace, active).is_some())
}

pub(in crate::workspace::terminal_agent) fn run_entry_count(run: &Run) -> usize {
    entry_count(
        run.status_transcript().map(|(trace, _)| trace),
        run.activity.len(),
        run.active(),
    )
}

fn typed(trace: Option<&Transcript>) -> Option<&Transcript> {
    trace.filter(|trace| !trace.steps.is_empty() || !trace.final_answer.trim().is_empty())
}

fn answer(trace: Option<&Transcript>, active: bool) -> Option<&str> {
    trace
        .filter(|_| !active)
        .map(|trace| trace.final_answer.trim())
        .filter(|answer| !answer.is_empty())
}

fn newest_age(
    trace: &Transcript,
    presentation: &Presentation,
    active: bool,
    now: Instant,
) -> Option<Duration> {
    let age = |time| now.checked_duration_since(time);
    let wall_age = |time| SystemTime::now().duration_since(time).ok();
    if let Some(review) = presentation.review_started {
        age(review)
    } else if answer(Some(trace), active).is_some() {
        trace
            .final_answer_at
            .and_then(wall_age)
            .or_else(|| presentation.finish_time().and_then(age))
    } else {
        let step = trace.steps.last()?;
        presentation
            .log_table
            .seen_at(Row::Step(step.id))
            .or(step.started_at)
            .and_then(age)
            .or_else(|| step.recorded_at.and_then(wall_age))
    }
}

pub(super) fn refresh_interval(
    trace: &Transcript,
    presentation: &Presentation,
    active: bool,
    now: Instant,
    reduced_motion: bool,
) -> Option<u64> {
    if !reduced_motion
        && !presentation.activity.expand_all
        && !presentation.card_stack_browsing()
        && newest_age(trace, presentation, active, now).is_some_and(|age| age < RISE)
    {
        Some(16)
    } else if active
        && trace
            .steps
            .iter()
            .any(|step| step.state == StepState::Running)
    {
        Some(1_000)
    } else {
        None
    }
}

#[derive(Clone, Copy)]
struct Motion {
    rise: f32,
    opacity: f32,
    recede: f32,
}

impl Motion {
    fn at(age: Option<Duration>) -> Self {
        let fraction = |duration: Duration| {
            age.map_or(1.0, |age| {
                (age.as_secs_f32() / duration.as_secs_f32()).min(1.0)
            })
        };
        let progress = fraction(RISE);
        let settle = fraction(RECEDE);
        Self {
            rise: 24.0 * (1.0 - progress).powi(2) * (progress * std::f32::consts::PI).cos(),
            opacity: fraction(FADE),
            recede: settle * settle * (3.0 - 2.0 * settle),
        }
    }

    fn behind(self, depth: usize) -> (f32, f32) {
        let from = 0.95_f32.powi(depth.saturating_sub(1) as i32);
        let to = from * 0.95;
        (
            STEP_BACK * (depth.saturating_sub(1) as f32 + self.recede),
            from + (to - from) * self.recede,
        )
    }
}

#[derive(Clone, Copy)]
enum Card<'a> {
    Step(&'a Step),
    Legacy(&'a str),
    Answer(&'a str),
}

fn card_at(activity: Activity<'_>, index: usize, active: bool) -> Option<Card<'_>> {
    if let Some(trace) = typed(activity.transcript) {
        trace.steps.get(index).map(Card::Step).or_else(|| {
            (index == trace.steps.len())
                .then(|| answer(activity.transcript, active))
                .flatten()
                .map(Card::Answer)
        })
    } else {
        activity.activity.get(index).map(|text| Card::Legacy(text))
    }
}

pub(super) fn view(activity: Activity<'_>, reduced_motion: bool) -> Element<'_, AgentMessage> {
    let active = activity.live_run.map_or_else(
        || {
            activity
                .transcript
                .is_some_and(|trace| !trace.state.finished())
        },
        Run::active,
    );
    let now = Instant::now();
    let approval = command_review::card(activity);
    let review = approval.is_some();
    let count = entry_count(activity.transcript, activity.activity.len(), active);
    let len = count + usize::from(review);
    if len == 0 {
        return history::detail(activity, 0);
    }
    if activity.presentation.activity.expand_all {
        let mut cards = column![].spacing(6).width(Length::Fill);
        for index in 0..count {
            if let Some(card) = card_at(activity, index, active) {
                cards = cards.push(front_card(activity, index, card, now));
            }
        }
        return column![scrollable(cards).height(Length::Fill), approval]
            .spacing(6)
            .height(Length::Fill)
            .into();
    }
    let back = activity.presentation.card_stack_back(len);
    let front = len - 1 - back;
    let age = if back == 0
        && len > 1
        && !reduced_motion
        && activity.live_run.is_none_or(|run| !run.reopened_history)
    {
        activity
            .transcript
            .and_then(|trace| newest_age(trace, activity.presentation, active, now))
    } else {
        None
    };
    let motion = Motion::at(age);
    let layers = front.min(BEHIND);
    let top = layers as f32 * STEP_BACK;
    let front_review = front == count && review;
    let front_card = if front_review {
        approval.expect("front review card")
    } else if let Some(card) = card_at(activity, front, active) {
        front_card(activity, front, card, now)
    } else {
        history::detail(activity, front)
    };
    // A pending review must be fully visible whenever its controls are active.
    let (rise, opacity) = if front_review {
        (0.0, 1.0)
    } else {
        (motion.rise, motion.opacity)
    };
    let face = mouse_area(fade::view(front_card, opacity)).interaction(mouse::Interaction::Idle);
    let mut cards = stack![positioned(face.into(), (top + rise).max(0.0))].width(Length::Fill);
    let browse = |back| AgentMessage::Design(Message::BrowseCards(back));
    for depth in 1..=layers {
        let index = front - depth;
        let Some(card) = card_at(activity, index, active) else {
            continue;
        };
        let (offset, scale) = motion.behind(depth);
        let heading = button(heading(activity, card, Some(index + 1)))
            .width(Length::Fill)
            .on_press(browse(len - 1 - index))
            .style(button::secondary);
        let background = card_menu(activity, index, card, heading.into());
        cards = cards.push_under(positioned(scale_card(background, scale), top - offset));
    }
    let scroll = move |direction| AgentMessage::Design(Message::ScrollCards { direction, review });
    let navigation = row![
        button("Previous").on_press_maybe((front > 0).then(|| scroll(1))),
        text(format!("Activity {} of {len}", front + 1)).size(activity.activity_font),
        button("Next").on_press_maybe((back > 0).then(|| scroll(-1))),
    ]
    .spacing(6)
    .wrap();
    let cards = column![navigation, cards].spacing(6).width(Length::Fill);
    scrollable(mouse_area(cards).on_scroll(move |delta| {
        let y = match delta {
            mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => y,
        };
        scroll(i8::from(y > 0.0) - i8::from(y < 0.0))
    }))
    .height(Length::Fill)
    .into()
}

fn positioned(content: Element<'_, AgentMessage>, top: f32) -> Element<'_, AgentMessage> {
    container(content)
        .width(Length::Fill)
        .padding(Padding {
            top,
            ..Padding::ZERO
        })
        .into()
}

fn heading<'a>(
    activity: Activity<'a>,
    card: Card<'a>,
    index: Option<usize>,
) -> Element<'a, AgentMessage> {
    let size = activity.activity_font;
    let (title, marker, role, mono) = match card {
        Card::Step(step) => {
            let marker = inline::marker(step, "›");
            (
                inline::clean(step.command.as_deref().unwrap_or(&step.title), false),
                marker.text,
                marker.role,
                step.kind == Kind::Action,
            )
        }
        Card::Legacy(text) => (history::summary(text), "·".into(), Role::Muted, false),
        Card::Answer(text) => (text.into(), "◆".into(), Role::Accent, false),
    };
    let title = match index {
        Some(index) => format!("{index} · {}", history::summary(&title)),
        None => title,
    };
    row![
        text(marker)
            .width(16)
            .size(size)
            .style(move |theme| marker_style(theme, role)),
        text(title)
            .size(size)
            .font(if mono { Font::MONOSPACE } else { Font::DEFAULT })
    ]
    .spacing(5)
    .width(Length::Fill)
    .into()
}

fn front_card<'a>(
    activity: Activity<'a>,
    index: usize,
    card: Card<'a>,
    now: Instant,
) -> Element<'a, AgentMessage> {
    let size = activity.activity_font;
    let mut body = column![].spacing(4).width(Length::Fill);
    match card {
        Card::Legacy(entry) => body = body.push(text(entry).size(size)),
        Card::Answer(_) => body = body.push(heading(activity, card, None)),
        Card::Step(step) => {
            body = body.push(heading(activity, card, None));
            if let Some(result) = metadata(step, now) {
                let role = inline::marker(step, "›").role;
                body = body.push(
                    text(result)
                        .size(size - 1.0)
                        .style(move |theme| marker_style(theme, role)),
                );
            }
            for line in &step.output[step.output.len().saturating_sub(OUTPUT_LINES)..] {
                body = body.push(
                    text(inline::clean(line, false))
                        .size(size - 1.0)
                        .font(Font::MONOSPACE),
                );
            }
        }
    }
    let face = container(body)
        .padding(9)
        .width(Length::Fill)
        .style(container::bordered_box);
    card_menu(activity, index, card, face.into())
}

fn card_menu<'a>(
    activity: Activity<'a>,
    index: usize,
    card: Card<'a>,
    content: Element<'a, AgentMessage>,
) -> Element<'a, AgentMessage> {
    match (activity.live_run, card) {
        (Some(run), Card::Step(step)) => {
            log_table::step_context(content, run, step, activity.surface.activity_entry(index))
        }
        (_, Card::Legacy(_)) => activity.entry_menu(index, content),
        _ => content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_motion_raises_the_front_and_recedes_the_older_card() {
        let entering = Motion::at(Some(Duration::ZERO));
        assert_eq!(entering.rise, 24.0);
        assert_eq!(entering.opacity, 0.0);
        assert_eq!(entering.behind(1), (0.0, 1.0));

        let settled = Motion::at(Some(RISE));
        assert_eq!(settled.rise, 0.0);
        assert_eq!(settled.opacity, 1.0);
        assert_eq!(settled.behind(1), (STEP_BACK, 0.95));

        let still = Motion::at(None);
        assert_eq!(still.rise, settled.rise);
        assert_eq!(still.opacity, settled.opacity);
        assert_eq!(still.behind(2), settled.behind(2));
    }
}
