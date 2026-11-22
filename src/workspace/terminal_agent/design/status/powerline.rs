//! The d01 bottom status bar and its typed step drawer.

use std::cell::Cell;
use std::time::{Duration, Instant};

use iced::widget::{button, canvas, column, container, responsive, row, scrollable, text, tooltip};
use iced::{
    Alignment, Color, Element, Font, Length, Point, Rectangle, Renderer, Size, Theme, mouse,
};

use super::{
    AgentMessage, Message, Status, StepState, Surface, Trace, bar_controls, history, palette,
    status_run_context, trace_drawer, usage_label,
};
use crate::workspace::terminal_agent::design::Presentation;

pub(in crate::workspace::terminal_agent::design) const HEIGHT: f32 = 24.0;
const VISIBLE_STEPS: usize = 8;
const REVEAL: Duration = Duration::from_millis(260);

#[derive(Default)]
pub(in crate::workspace::terminal_agent::design) struct Drawer {
    manual: Cell<Option<Instant>>,
    dismissed_review: Cell<Option<Instant>>,
}

impl Drawer {
    pub(in crate::workspace::terminal_agent::design) fn opened_at(
        &self,
        review: Option<Instant>,
    ) -> Option<Instant> {
        self.manual
            .get()
            .or_else(|| review.filter(|started| Some(*started) != self.dismissed_review.get()))
    }

    pub(in crate::workspace::terminal_agent::design) fn dismiss(&self, review: Option<Instant>) {
        self.manual.set(None);
        self.dismissed_review.set(review);
    }

    pub(in crate::workspace::terminal_agent::design) fn toggle(
        &self,
        review: Option<Instant>,
        now: Instant,
    ) {
        if self.opened_at(review).is_some() {
            self.dismiss(review);
        } else {
            self.manual.set(Some(now));
        }
    }
}

fn reveal(opened_at: Instant, now: Instant, reduced_motion: bool) -> f32 {
    if reduced_motion {
        return 1.0;
    }
    let progress =
        (now.saturating_duration_since(opened_at).as_secs_f32() / REVEAL.as_secs_f32()).min(1.0);
    1.0 - (1.0 - progress).powi(3)
}

pub(in crate::workspace::terminal_agent::design) fn refresh_interval(
    presentation: &Presentation,
    active: bool,
    paused: bool,
    now: Instant,
    reduced_motion: bool,
) -> Option<u64> {
    if presentation
        .powerline_drawer_at()
        .is_some_and(|opened| reveal(opened, now, reduced_motion) < 1.0)
    {
        Some(16)
    } else if active && !paused && !reduced_motion {
        Some(100)
    } else {
        active.then_some(1000)
    }
}

fn current_step(trace: &Trace<'_>, phase: &str) -> String {
    if trace.waiting {
        return phase.to_owned();
    }
    if !trace.active && !trace.answer.trim().is_empty() {
        return history::summary(trace.answer);
    }
    if trace.run.is_some() && phase != "Working" {
        return phase.to_owned();
    }
    let step = trace
        .steps
        .iter()
        .rfind(|step| step.state == StepState::Running)
        .or_else(|| trace.steps.last());
    step.map_or_else(
        || phase.to_owned(),
        |step| history::summary(step.command.as_deref().unwrap_or(&step.title)),
    )
}

fn colors(theme: &Theme) -> [Color; 4] {
    let palette = theme.palette();
    let accent = palette.primary;
    let background = palette.background;
    let shade = |amount: f32| {
        Color::from_rgb(
            accent.r + (background.r - accent.r) * amount,
            accent.g + (background.g - accent.g) * amount,
            accent.b + (background.b - accent.b) * amount,
        )
    };
    [accent, shade(0.55), shade(0.78), background]
}

fn segment<'a>(label: String, index: usize, width: Length) -> Element<'a, AgentMessage> {
    container(
        text(label)
            .font(Font::MONOSPACE)
            .size(12)
            .wrapping(text::Wrapping::None),
    )
    .width(width)
    .center_y(HEIGHT)
    .padding([0, 6])
    .clip(true)
    .style(move |theme| {
        let color = colors(theme)[index];
        let text = iced::theme::palette::Pair::new(color, theme.palette().text).text;
        container::Style::default().background(color).color(text)
    })
    .into()
}

struct Arrow {
    from: usize,
    to: usize,
}

impl canvas::Program<AgentMessage> for Arrow {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let colors = colors(theme);
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), colors[self.to]);
        frame.fill(
            &canvas::Path::new(|path| {
                path.move_to(Point::ORIGIN);
                path.line_to(Point::new(bounds.width, bounds.height / 2.0));
                path.line_to(Point::new(0.0, bounds.height));
                path.close();
            }),
            colors[self.from],
        );
        vec![frame.into_geometry()]
    }
}

fn arrow(from: usize, to: usize) -> canvas::Canvas<Arrow, AgentMessage> {
    canvas::Canvas::new(Arrow { from, to })
        .width(9)
        .height(HEIGHT)
}

pub(super) fn view<'a>(
    activity: history::Activity<'a>,
    phase: &str,
    trace: Trace<'a>,
    palette: Option<palette::Palette>,
    has_archive: bool,
    sample_controls: impl Fn() -> Element<'a, AgentMessage> + 'a,
) -> Element<'a, AgentMessage> {
    let opened = activity.presentation.powerline_drawer_at();
    let drawer = opened.is_some();
    let current = current_step(&trace, phase);
    let spinner = trace
        .spinner()
        .map_or_else(String::new, |symbol| format!("{symbol} "));
    let commands = trace
        .steps
        .iter()
        .filter(|step| step.command.is_some())
        .count();
    let count = format!(
        "{commands} {}",
        if commands == 1 { "command" } else { "commands" }
    );
    let preview_tokens = if trace.run.is_none() {
        activity.transcript.and_then(|transcript| transcript.tokens)
    } else {
        None
    };
    let header = move |size: Size| {
        let compact = size.width < 560.0;
        let show_count = size.width >= 355.0;
        let label = segment(format!("{spinner}{current}"), 1, Length::Fill);
        let mut summary = column![
            text(current.clone()),
            text(format!("{count} · {}", trace.elapsed_label()))
        ]
        .spacing(3);
        if let Some(usage) = activity.presentation.model_usage.compact() {
            summary = summary.push(text(usage));
        } else if let Some(tokens) = preview_tokens {
            summary = summary.push(text(format!("{tokens} tokens")));
        }
        let label = container(tooltip(
            label,
            container(summary).max_width(420).padding(4),
            tooltip::Position::Top,
        ))
        .width(Length::Fill);
        let mut header = row![
            segment(
                if compact { "◆" } else { "◆ Agent" }.into(),
                0,
                Length::Shrink
            ),
            arrow(0, 1),
            label,
        ]
        .align_y(Alignment::Center)
        .width(Length::Fill);
        if show_count {
            header = header
                .push(arrow(1, 2))
                .push(segment(count.clone(), 2, Length::Shrink))
                .push(arrow(2, 3));
        } else {
            header = header.push(arrow(1, 3));
        }
        if !compact {
            header = header.push(container(text(trace.elapsed_label()).size(11)).padding([0, 5]));
            if let Some(usage) = usage_label(activity, palette) {
                header = header.push(usage);
            } else if let Some(tokens) = preview_tokens {
                header = header.push(text(format!("{tokens} tokens")).size(11));
            }
        }
        let toggle = Message::TogglePowerlineDrawer;
        if let Some(controls) = bar_controls(&trace, has_archive, drawer, compact, toggle) {
            if compact {
                let bar = scrollable::Scrollbar::new().width(2).scroller_width(2);
                let actions = scrollable(controls)
                    .direction(scrollable::Direction::Horizontal(bar))
                    .width(Length::Shrink);
                let reserve = if show_count { 172.0 } else { 75.0 };
                header = header.push(container(actions).max_width((size.width - reserve).max(0.0)));
            } else {
                header = header.push(controls);
            }
        } else {
            header = header.push(
                button(text(if drawer { "Close details" } else { "Details" }).size(12))
                    .style(button::text)
                    .padding(3)
                    .on_press(AgentMessage::Design(toggle)),
            );
        }
        status_run_context(
            container(header)
                .width(Length::Fill)
                .center_y(HEIGHT)
                .clip(true)
                .style(|theme| container::Style::default().background(theme.palette().background))
                .into(),
            activity,
            Status::Powerline,
        )
    };
    let panel = responsive(move |size| {
        let mut panel = column![].width(Length::Fill);
        if let Some(opened) = opened {
            let older = trace.steps.len().saturating_sub(VISIBLE_STEPS);
            let visible = Trace {
                steps: &trace.steps[older..],
                ..trace
            };
            let mut contents = column![].spacing(3);
            if older != 0 {
                contents = contents.push(text(format!("+{older} earlier steps")).size(11));
            }
            contents = contents.push(trace_drawer(
                &visible,
                activity.presentation.selected_step,
                Length::Fill,
                Surface::StatusStepList,
            ));
            if trace.run.is_none() && trace.waiting {
                contents = contents.push(sample_controls());
            }
            let available = (size.height - HEIGHT).clamp(0.0, 164.0);
            let height = reveal(opened, trace.now, trace.reduced_motion) * available;
            let drawer = container(contents).padding(5).width(Length::Fill);
            panel = panel.push(
                container(palette::panel(palette, drawer))
                    .height(height)
                    .max_width(760)
                    .clip(true),
            );
        }
        panel.push(container(header(size)).height(HEIGHT)).into()
    })
    .height(Length::Shrink);
    palette::scope(palette, panel.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_agent::design::inline::{State, Step, Transcript};

    #[test]
    fn review_drawer_closes_on_resolution_and_preserves_manual_opening() {
        let now = Instant::now();
        let later = now + REVEAL;
        let drawer = Drawer::default();
        assert_eq!(drawer.opened_at(Some(now)), Some(now));
        assert_eq!(drawer.opened_at(None), None);
        drawer.toggle(Some(now), later);
        assert_eq!(drawer.opened_at(Some(now)), None);
        assert_eq!(drawer.opened_at(Some(later)), Some(later));
        drawer.toggle(Some(now), later);
        assert_eq!(drawer.opened_at(None), Some(later));
        drawer.dismiss(None);
        assert_eq!(drawer.opened_at(None), None);
        assert_eq!(reveal(now, now + REVEAL / 2, false), 0.875);
        assert_eq!(reveal(now, now, true), 1.0);
    }

    #[test]
    fn powerline_names_the_running_command_then_the_final_answer() {
        let now = Instant::now();
        let mut transcript = Transcript {
            steps: vec![
                Step {
                    command: Some("printf fixture".into()),
                    state: StepState::Running,
                    ..Step::default()
                },
                Step {
                    title: "An older result arrived".into(),
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        let trace = Trace::sample(&transcript, now, now, false);
        assert_eq!(current_step(&trace, "Working"), "printf fixture");
        assert_eq!(
            current_step(
                &Trace {
                    waiting: true,
                    ..trace
                },
                "Review the command"
            ),
            "Review the command"
        );
        transcript.state = State::Finished;
        transcript.final_answer = "Completed fixture".into();
        assert_eq!(
            current_step(&Trace::sample(&transcript, now, now, false), "Completed"),
            "Completed fixture"
        );
    }
}
