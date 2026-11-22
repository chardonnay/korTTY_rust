//! The compact d11 rail and the previews for its individually addressed steps.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use iced::widget::{button, column, container, mouse_area, responsive, scrollable, space, text};
use iced::{Alignment, Color, Element, Length, Shadow, Theme, Vector};

use crate::terminal_agent::design::Status;
use crate::terminal_agent::design::inline::{Kind, Step, StepState, Transcript};
use crate::workspace::chrome::popup;

use super::{
    AgentMessage, Message, Surface, Trace, history, palette, status_run_context, step_context,
};
use crate::workspace::terminal_agent::design::log_table;

pub(in crate::workspace::terminal_agent::design) const WIDTH: f32 = 14.0;
const GAP: f32 = 3.0;
const GROW: Duration = Duration::from_millis(260);
const LENS: Duration = Duration::from_millis(160);
const GLOW: Duration = Duration::from_millis(1400);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Step(u64),
    Legacy(usize),
}

#[derive(Default)]
pub(in crate::workspace::terminal_agent::design) struct Motion {
    appeared: Vec<(u64, Instant)>,
    hovered: Option<(Target, Instant)>,
}

impl Motion {
    pub(in crate::workspace::terminal_agent::design) fn sync(
        &mut self,
        transcript: &Transcript,
        now: Instant,
    ) {
        self.appeared
            .retain(|(id, _)| transcript.steps.iter().any(|step| step.id == *id));
        for step in &transcript.steps {
            if !self.appeared.iter().any(|(id, _)| *id == step.id) {
                self.appeared.push((step.id, now));
            }
        }
        if let Some((Target::Step(id), _)) = self.hovered
            && !self.appeared.iter().any(|(current, _)| *current == id)
        {
            self.hovered = None;
        }
    }

    pub(in crate::workspace::terminal_agent::design) fn hover(
        &mut self,
        target: Target,
        entered: bool,
    ) {
        let same = self.hovered.is_some_and(|(current, _)| current == target);
        if entered && !same {
            self.hovered = Some((target, Instant::now()));
        } else if !entered && same {
            self.hovered = None;
        }
    }

    pub(in crate::workspace::terminal_agent::design) fn animating(
        &self,
        transcript: &Transcript,
        active: bool,
        now: Instant,
        reduced: bool,
    ) -> bool {
        !reduced
            && (active
                && transcript
                    .steps
                    .iter()
                    .any(|step| step.state == StepState::Running)
                || self
                    .appeared
                    .iter()
                    .any(|(_, at)| now.saturating_duration_since(*at) < GROW)
                || self
                    .hovered
                    .is_some_and(|(_, at)| now.saturating_duration_since(at) < LENS))
    }

    fn growth(&self, target: Target, now: Instant, reduced: bool) -> f32 {
        match target {
            Target::Step(id) => ease(
                self.appeared
                    .iter()
                    .find(|(key, _)| *key == id)
                    .map(|(_, at)| *at),
                now,
                GROW,
                reduced,
            ),
            Target::Legacy(_) => 1.0,
        }
    }

    fn lens(&self, target: Target, now: Instant, reduced: bool) -> f32 {
        let started = self
            .hovered
            .filter(|(id, _)| *id == target)
            .map(|(_, at)| at);
        ease(started, now, LENS, reduced)
    }
}

fn ease(at: Option<Instant>, now: Instant, duration: Duration, reduced: bool) -> f32 {
    at.filter(|_| !reduced).map_or(1.0, |at| {
        let progress =
            (now.saturating_duration_since(at).as_secs_f32() / duration.as_secs_f32()).min(1.0);
        1.0 - (1.0 - progress).powi(3)
    })
}

fn segment_height(height: f32, count: usize) -> f32 {
    ((height - GAP) / count.max(1) as f32 - GAP).clamp(5.0, 28.0)
}

pub(super) fn ink(step: Option<&Step>, theme: &Theme) -> Color {
    let palette = theme.palette();
    let Some(step) = step else {
        return palette.text.scale_alpha(0.7);
    };
    if step.state == StepState::Failed || step.kind == Kind::Error {
        return palette.danger;
    }
    if step.state == StepState::Cancelled {
        return palette.text.scale_alpha(0.4);
    }
    match step.kind {
        Kind::Action => palette.primary,
        Kind::Question => palette.warning,
        Kind::Thinking if iced::theme::palette::is_dark(palette.background) => {
            Color::from_rgb8(0xbe, 0x9c, 0xfa)
        }
        Kind::Thinking => Color::from_rgb8(0x6f, 0x45, 0xb3),
        _ => palette.text.scale_alpha(0.7),
    }
}

fn excerpt<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    lines
        .take(4)
        .map(|line| line.chars().take(140).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, Copy)]
enum Item<'a> {
    Step(&'a Step),
    Legacy(usize, &'a str),
}

fn segment<'a>(
    activity: history::Activity<'a>,
    trace: &Trace<'a>,
    item: Item<'a>,
    height: f32,
    phase: &'static str,
) -> Element<'a, AgentMessage> {
    let (target, step, title, result, detail, select) = match item {
        Item::Step(step) => {
            let result = step.exit_code.map_or_else(
                || log_table::state_label(step).to_owned(),
                |code| format!("Exit code {code}"),
            );
            (
                Target::Step(step.id),
                Some(step),
                history::summary(step.command.as_deref().unwrap_or(&step.title)),
                Some(format!("{} · {result}", log_table::kind(step))),
                if step.output.is_empty() {
                    excerpt(step.detail.lines())
                } else {
                    excerpt(step.output.iter().map(String::as_str))
                },
                Message::SelectStep(step.id),
            )
        }
        Item::Legacy(index, entry) => {
            let identity = activity.presentation.identity(index);
            (
                Target::Legacy(identity),
                None,
                history::summary(entry),
                None,
                excerpt(entry.lines()),
                Message::Select(identity),
            )
        }
    };
    let motion = &activity.presentation.minimap;
    let scale = motion.growth(target, trace.now, trace.reduced_motion);
    let running = trace.active && step.is_some_and(|step| step.state == StepState::Running);
    let glow = if running && !trace.reduced_motion && !trace.paused {
        let phase = trace.animation_age.as_secs_f32() % GLOW.as_secs_f32() / GLOW.as_secs_f32();
        3.0 + 4.0 * (1.0 - (phase * TAU).cos())
    } else if running {
        3.0
    } else {
        0.0
    };
    let marker = container(space::vertical().height(height * scale))
        .width(8)
        .style(move |theme| {
            let color = ink(step, theme);
            container::Style {
                background: Some(color.into()),
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                shadow: Shadow {
                    color: color.scale_alpha(if running { 0.7 } else { 0.0 }),
                    offset: Vector::ZERO,
                    blur_radius: glow,
                },
                ..Default::default()
            }
        });
    let trigger = button(container(marker).height(height).center_y(height))
        .padding(0)
        .style(button::text)
        .on_press(AgentMessage::Design(select));
    let trigger = mouse_area(trigger)
        .on_enter(AgentMessage::Design(Message::MinimapHover(target, true)))
        .on_exit(AgentMessage::Design(Message::MinimapHover(target, false)));
    let lens = (motion.hovered.is_some_and(|(id, _)| id == target)
        && !activity.presentation.log_table.minimap_menu_open())
    .then(|| {
        let lens = column![
            text(title).size(13),
            result.map(|label| text(label).size(12)),
            (!detail.is_empty()).then(|| text(detail).size(12)),
            text(phase).size(12),
            activity
                .presentation
                .model_usage
                .compact()
                .map(|usage| text(usage).size(11)),
        ]
        .spacing(3);
        let fraction = motion.lens(target, trace.now, trace.reduced_motion);
        let lens = container(lens)
            .width(272.0 + 48.0 * fraction)
            .padding(7)
            .style(container::bordered_box)
            .into();
        crate::workspace::terminal_agent::design::fade::view(lens, 0.7 + 0.3 * fraction)
    });
    let trigger = popup::preview_left(trigger.into(), lens);
    match (item, trace.run) {
        (Item::Step(step), Some(run)) => {
            step_context(trigger, run, step, Surface::MinimapStep(step.id))
        }
        (Item::Legacy(index, _), _) => activity.step_context(
            index,
            Surface::Minimap(activity.presentation.identity(index)),
            trigger,
        ),
        _ => trigger,
    }
}

pub(super) fn view<'a>(
    activity: history::Activity<'a>,
    trace: Trace<'a>,
    phase: &'static str,
    palette: Option<palette::Palette>,
) -> Element<'a, AgentMessage> {
    let rail = responsive(move |size| {
        let typed = !trace.steps.is_empty();
        let count = if typed {
            trace.steps.len()
        } else {
            activity.activity.len()
        };
        let height = segment_height(size.height, count);
        let mut content = column![]
            .spacing(GAP)
            .padding([3, 0])
            .width(Length::Fill)
            .align_x(Alignment::Center);
        if typed {
            for step in trace.steps {
                content = content.push(segment(activity, &trace, Item::Step(step), height, phase));
            }
        } else {
            for (index, entry) in activity.activity.iter().enumerate() {
                content = content.push(segment(
                    activity,
                    &trace,
                    Item::Legacy(index, entry),
                    height,
                    phase,
                ));
            }
        }
        scrollable(content)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::hidden(),
            ))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    });
    palette::panel(
        palette,
        container(status_run_context(rail.into(), activity, Status::Minimap))
            .width(WIDTH)
            .height(Length::Fill),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimap_preserves_hit_targets_and_reveals_the_newest_typed_steps() {
        let now = Instant::now();
        let mut motion = Motion::default();
        let transcript = Transcript {
            steps: vec![Step {
                id: 42,
                state: StepState::Running,
                ..Step::default()
            }],
            ..Transcript::default()
        };
        let target = Target::Step(42);
        motion.sync(&transcript, now);
        motion.sync(&transcript, now + GROW / 2);
        assert_eq!(motion.growth(target, now, false), 0.0);
        assert!((0.0..1.0).contains(&motion.growth(target, now + GROW / 2, false)));
        assert_eq!(motion.growth(target, now + GROW, false), 1.0);
        assert_eq!(motion.growth(target, now, true), 1.0);
        assert!(motion.animating(&transcript, true, now + GROW, false));
        assert!(!motion.animating(&transcript, false, now + GROW, false));
        assert!(!motion.animating(&transcript, true, now, true));
        assert_eq!(segment_height(20.0, 15), 5.0);
        assert_eq!(segment_height(400.0, 2), 28.0);
        motion.hover(target, true);
        motion.sync(&Transcript::default(), now + GROW);
        assert!(motion.hovered.is_none());
    }
}
