//! A compact d04 card and its expandable step list over the terminal.

use std::cell::Cell;
use std::time::{Duration, Instant};

use iced::widget::{button, canvas, column, container, row, space, text, tooltip};
use iced::{Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, mouse};

use super::{
    AgentMessage, Message, Surface, Trace, history, minimap, palette, status_run_context,
    trace_drawer, usage_label,
};
use crate::terminal_agent::design::Status;
use crate::terminal_agent::design::inline::{Step, StepState};
use crate::workspace::terminal_agent::design::{Presentation, actions, fade};

const WIDTH: f32 = 300.0;
const TYPING_OPACITY: f32 = 0.35;
const TYPING_IDLE: Duration = Duration::from_millis(1_400);
const FADE: Duration = Duration::from_millis(160);

#[derive(Default)]
pub(in crate::workspace::terminal_agent::design) struct Typing(Cell<Option<Fade>>);

#[derive(Clone, Copy)]
struct Fade {
    start: Instant,
    from: f32,
    last_key: Instant,
}

impl Typing {
    pub(in crate::workspace::terminal_agent::design) fn key(&self, now: Instant) {
        let fade = self
            .0
            .get()
            .filter(|fade| now < fade.last_key + TYPING_IDLE)
            .map_or_else(
                || Fade {
                    start: now,
                    from: self.opacity(now, false),
                    last_key: now,
                },
                |fade| Fade {
                    last_key: now,
                    ..fade
                },
            );
        self.0.set(Some(fade));
    }

    pub(in crate::workspace::terminal_agent::design) fn reset(&self) {
        self.0.set(None);
    }

    fn opacity(&self, now: Instant, reduced_motion: bool) -> f32 {
        let Some(fade) = self.0.get() else { return 1.0 };
        let idle = fade.last_key + TYPING_IDLE;
        if reduced_motion {
            return if now < idle { TYPING_OPACITY } else { 1.0 };
        }
        let (start, from, to) = if now < idle {
            (fade.start, fade.from, TYPING_OPACITY)
        } else {
            (idle, TYPING_OPACITY, 1.0)
        };
        let progress =
            (now.saturating_duration_since(start).as_secs_f32() / FADE.as_secs_f32()).min(1.0);
        let eased = progress * progress * (3.0 - 2.0 * progress);
        from + (to - from) * eased
    }

    fn refresh_interval(&self, now: Instant, reduced_motion: bool) -> Option<u64> {
        let fade = self.0.get()?;
        let idle = fade.last_key + TYPING_IDLE;
        if reduced_motion {
            return (now < idle).then_some(100);
        }
        if now < fade.start + FADE || (idle..idle + FADE).contains(&now) {
            Some(16)
        } else {
            (now < idle).then_some(100)
        }
    }
}

pub(in crate::workspace::terminal_agent::design) fn refresh_interval(
    presentation: &Presentation,
    active: bool,
    paused: bool,
    now: Instant,
    reduced_motion: bool,
) -> Option<u64> {
    presentation
        .hud_typing
        .refresh_interval(now, reduced_motion)
        .into_iter()
        .chain(active.then_some(if paused || reduced_motion { 1000 } else { 100 }))
        .min()
}

struct Segments<'a> {
    steps: &'a [Step],
    pulse: bool,
}

impl<M> canvas::Program<M> for Segments<'_> {
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
        if self.steps.is_empty() {
            frame.fill_rectangle(
                Point::ORIGIN,
                bounds.size(),
                theme.palette().text.scale_alpha(0.2),
            );
        } else {
            let pitch = bounds.width / self.steps.len() as f32;
            let gap = 3.0_f32.min(pitch * 0.2);
            for (index, step) in self.steps.iter().enumerate() {
                let x = index as f32 * pitch;
                let segment = Size::new(
                    if index + 1 == self.steps.len() {
                        pitch
                    } else {
                        pitch - gap
                    },
                    bounds.height,
                );
                let ink = minimap::ink(Some(step), theme);
                let ink = if self.pulse && step.state == StepState::Running {
                    Color::from_rgba(
                        ink.r * 0.7 + 0.3,
                        ink.g * 0.7 + 0.3,
                        ink.b * 0.7 + 0.3,
                        ink.a,
                    )
                } else {
                    ink
                };
                frame.fill_rectangle(Point::new(x, 0.0), segment, ink);
            }
        }
        vec![frame.into_geometry()]
    }
}

fn single_line<'a>(value: String, mono: bool) -> Element<'a, AgentMessage> {
    let mut label = text(value).size(12).wrapping(text::Wrapping::None);
    if mono {
        label = label.font(iced::Font::MONOSPACE);
    }
    container(label).width(Length::Fill).clip(true).into()
}

pub(super) fn view<'a>(
    activity: history::Activity<'a>,
    phase: &str,
    trace: &Trace<'a>,
    palette: Option<palette::Palette>,
    has_archive: bool,
    sample_controls: impl Fn() -> Element<'a, AgentMessage>,
) -> Element<'a, AgentMessage> {
    let open = activity.presentation.drawer_open();
    let animated = trace.active && !trace.paused && !trace.reduced_motion;
    let frame = (trace.animation_age.as_millis() / 100) as usize;
    let details = button(if open { "Close details" } else { "Details" })
        .style(button::text)
        .padding(3)
        .on_press(AgentMessage::Design(Message::ToggleDrawer));
    let menu = trace
        .run
        .and_then(|run| actions::run_menu(run, has_archive));
    let header = row![text("◆ Agent").size(13), space::horizontal(), details, menu]
        .spacing(4)
        .align_y(Alignment::Center);
    let step = trace.current_label(phase);
    let step = if trace.waiting || !trace.active || trace.paused {
        step
    } else {
        let spinner = ["◐", "◓", "◑", "◒"];
        format!("{} {step}", spinner[if animated { frame % 4 } else { 0 }])
    };
    let (done, total) = trace.progress();
    let mut footer = row![
        text(if trace.waiting {
            "Waiting for input".to_owned()
        } else {
            format!("{done}/{total} steps · {}", trace.elapsed_label())
        })
        .size(11),
        space::horizontal(),
        usage_label(activity, palette),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    if trace.run.is_some() {
        let label = if trace.waiting {
            "Review…"
        } else {
            "Activity"
        };
        footer = footer.push(tooltip(
            button(label)
                .style(button::text)
                .padding(3)
                .on_press(AgentMessage::Focus),
            text("Open Agent Activity").size(11),
            tooltip::Position::Top,
        ));
    }
    let alpha = if trace.reduced_motion { 0.82 } else { 0.55 };
    let card = container(
        column![
            header,
            single_line(history::summary(activity.goal), false),
            single_line(step, true),
            canvas(Segments {
                steps: trace.steps,
                pulse: animated && frame % 10 < 5
            })
            .width(Length::Fill)
            .height(4),
            footer,
        ]
        .spacing(5),
    )
    .padding([8, 10])
    .width(Length::Fill)
    .style(move |theme| {
        let mut style = container::bordered_box(theme);
        style.background = Some(theme.palette().background.scale_alpha(alpha).into());
        style.border.radius = fade::CORNER_RADIUS.into();
        style
    });
    let card = status_run_context(card.into(), activity, Status::Hud);
    let opacity = trace.opacity
        * activity
            .presentation
            .hud_typing
            .opacity(trace.now, trace.reduced_motion);
    let entrance = &activity.presentation.status_visibility.hud_entrance;
    let card = if trace.reduced_motion {
        entrance.settle();
        fade::view(card, opacity)
    } else {
        fade::frosted(card, opacity, &activity.presentation.hud_glass, entrance)
    };
    let mut panel = column![].width(WIDTH).spacing(6);
    if open {
        let list = trace_drawer(
            trace,
            activity.presentation.selected_step,
            144.into(),
            Surface::StatusStepList,
        );
        panel = panel.push(fade::view(
            palette::panel(palette, container(list).width(Length::Fill).padding(5)),
            trace.opacity,
        ));
        if trace.waiting && trace.run.is_none() {
            panel = panel.push(fade::view(sample_controls(), trace.opacity));
        }
    }
    palette::scope(palette, panel.push(card).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_fades_and_keeps_the_card_dim_until_idle_after_the_last_key() {
        let typing = Typing::default();
        let now = Instant::now();
        assert_eq!(typing.opacity(now, false), 1.0);
        typing.key(now);
        assert_eq!(typing.opacity(now, false), 1.0);
        assert_eq!(typing.refresh_interval(now, false), Some(16));
        assert_eq!(typing.opacity(now, true), TYPING_OPACITY);
        let next = now + Duration::from_millis(50);
        let fading = typing.opacity(next, false);
        typing.key(next);
        assert!(fading > TYPING_OPACITY && fading < 1.0);
        assert_eq!(typing.opacity(next, false), fading);
        assert!((typing.opacity(now + FADE, false) - TYPING_OPACITY).abs() < f32::EPSILON);
        assert!((typing.opacity(now + TYPING_IDLE, false) - TYPING_OPACITY).abs() < f32::EPSILON);
        let idle = next + TYPING_IDLE;
        assert_eq!(typing.opacity(idle, true), 1.0);
        assert_eq!(typing.refresh_interval(idle, false), Some(16));
        assert_eq!(typing.opacity(idle + FADE, false), 1.0);
        assert_eq!(typing.refresh_interval(idle + FADE, false), None);
    }
}
