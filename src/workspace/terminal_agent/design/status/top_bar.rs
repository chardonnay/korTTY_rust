//! The d12 status track and step drawer shared by live and preview panes.

use std::time::{Duration, Instant};

use iced::widget::{button, column, container, responsive, row, scrollable, text, tooltip};
use iced::{Alignment, Background, Color, Element, Length, Theme, gradient};

use super::{
    AgentMessage, Message, Surface, Tone, Trace, bar_controls, history, palette, trace_drawer,
    usage_label,
};
use crate::workspace::terminal_agent::design::Presentation;

pub(in crate::workspace::terminal_agent::design) const HEIGHT: f32 = 38.0;
const DRAWER_HEIGHT: f32 = 148.0;
const COMPACT_WIDTH: f32 = 520.0;
const COMPACT_TRACK_WIDTH: f32 = 68.0;
const REVEAL: Duration = Duration::from_millis(260);
const SWEEP: Duration = Duration::from_millis(1400);

fn reveal(opened_at: Option<Instant>, now: Instant, reduced_motion: bool) -> f32 {
    if reduced_motion {
        return 1.0;
    }
    opened_at.map_or(1.0, |opened_at| {
        let progress = (now.saturating_duration_since(opened_at).as_secs_f32()
            / REVEAL.as_secs_f32())
        .min(1.0);
        1.0 - (1.0 - progress).powi(3)
    })
}

pub(in crate::workspace::terminal_agent::design) fn refresh_interval(
    presentation: &Presentation,
    active: bool,
    paused: bool,
    now: Instant,
    reduced_motion: bool,
) -> Option<u64> {
    let opening = presentation.drawer_open()
        && reveal(presentation.drawer_opened_at(), now, reduced_motion) < 1.0;
    if opening || active && !paused && !reduced_motion {
        Some(16)
    } else {
        active.then_some(1000)
    }
}

fn track_background(theme: &Theme, tone: Tone, waiting: bool, sweep: Option<f32>) -> Background {
    let colors = theme.palette();
    let base = match tone {
        Tone::Finished => return colors.success.into(),
        Tone::Stopped => return colors.danger.into(),
        Tone::Active if waiting => colors.warning,
        Tone::Active => colors.primary,
    };
    let Some(sweep) = sweep else {
        return base.into();
    };
    let light = Color::from_rgb(base.r * 0.4 + 0.6, base.g * 0.4 + 0.6, base.b * 0.4 + 0.6);
    let center = 0.005 + 0.99 * sweep;
    gradient::Linear::new(iced::Degrees(90.0))
        .add_stop((center - 0.22).max(0.0), base)
        .add_stop(center, light)
        .add_stop((center + 0.22).min(1.0), base)
        .into()
}

fn one_line<'a>(value: String, size: u32) -> Element<'a, AgentMessage> {
    container(text(value).size(size).wrapping(text::Wrapping::None))
        .width(Length::Fill)
        .clip(true)
        .into()
}

pub(super) fn view<'a>(
    activity: history::Activity<'a>,
    phase: &str,
    trace: Trace<'a>,
    palette: Option<palette::Palette>,
    has_archive: bool,
    sample_controls: impl Fn() -> Element<'a, AgentMessage>,
) -> Element<'a, AgentMessage> {
    let sweep = (trace.active && !trace.paused && !trace.reduced_motion)
        .then(|| trace.animation_age.as_secs_f32() % SWEEP.as_secs_f32() / SWEEP.as_secs_f32());
    let tone = trace.tone;
    let waiting = trace.waiting;
    let drawer = activity.presentation.drawer_open();
    let commands = trace
        .steps
        .iter()
        .filter(|step| step.command.is_some())
        .count();
    let unit = if commands == 1 { "command" } else { "commands" };
    let current = format!("{commands} {unit} · {}", trace.current_label(phase));
    let goal = history::summary(activity.goal);
    let header = responsive(move |size| {
        let compact = size.width < COMPACT_WIDTH;
        let track = container(iced::widget::space::vertical().height(4))
            .width(Length::Fill)
            .style(move |theme| {
                container::Style::default()
                    .background(track_background(theme, tone, waiting, sweep))
            });
        let label = column![track, one_line(current.clone(), 11)].spacing(2);
        let mut header = if compact {
            let mut summary = column![
                text(goal.clone()).size(12),
                text(current.clone()).size(12),
                text(trace.elapsed_label()).size(11),
            ]
            .spacing(3);
            if let Some(usage) = activity.presentation.model_usage.compact() {
                summary = summary.push(text(usage).size(11));
            }
            row![
                container(tooltip(
                    label,
                    container(summary).padding(5).max_width(420),
                    tooltip::Position::Bottom,
                ))
                .width(Length::Fill)
            ]
        } else {
            let mut wide = row![
                container(one_line(goal.clone(), 12)).width(Length::FillPortion(2)),
                container(label).width(Length::FillPortion(3)),
                text(trace.elapsed_label()).size(11),
            ];
            if let Some(usage) = usage_label(activity, palette) {
                wide = wide.push(usage);
            }
            wide
        }
        .spacing(6)
        .align_y(Alignment::Center);
        if let Some(controls) =
            bar_controls(&trace, has_archive, drawer, compact, Message::ToggleDrawer)
        {
            let activity_button = || {
                tooltip(
                    button("Activity")
                        .style(button::text)
                        .padding(3)
                        .on_press(AgentMessage::Focus),
                    text("Activity window").size(12),
                    tooltip::Position::Bottom,
                )
            };
            if compact {
                let mut actions = controls;
                if !waiting {
                    actions = actions.push(activity_button());
                }
                let bar = scrollable::Scrollbar::new().width(2).scroller_width(2);
                let actions = scrollable(actions)
                    .direction(scrollable::Direction::Horizontal(bar))
                    .width(Length::Shrink);
                header = header.push(
                    container(actions).max_width((size.width - COMPACT_TRACK_WIDTH).max(0.0)),
                );
            } else {
                if !waiting {
                    header = header.push(activity_button());
                }
                header = header.push(controls);
            }
        } else {
            header = header.push(
                button(if drawer { "Close details" } else { "Details" })
                    .style(button::text)
                    .on_press(AgentMessage::Design(Message::ToggleDrawer)),
            );
        }
        palette::panel(
            palette,
            container(header)
                .width(Length::Fill)
                .center_y(HEIGHT)
                .padding([0, 6])
                .clip(true),
        )
    })
    .height(HEIGHT);
    let mut panel = column![header].width(Length::Fill);
    if drawer {
        let list = trace_drawer(
            &trace,
            activity.presentation.selected_step,
            Length::Fixed(DRAWER_HEIGHT),
            Surface::StatusStepList,
        );
        let height = reveal(
            activity.presentation.drawer_opened_at(),
            trace.now,
            trace.reduced_motion,
        ) * DRAWER_HEIGHT;
        let list = palette::panel(palette, container(list).padding(5).width(Length::Fill));
        panel = panel.push(container(list).height(height).max_width(760).clip(true));
        if trace.run.is_none() && trace.waiting {
            panel = panel.push(palette::panel(
                palette,
                container(sample_controls()).padding(4),
            ));
        }
    }
    panel.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_moves_and_settled_colors_take_precedence() {
        let theme = Theme::Dark;
        let early = track_background(&theme, Tone::Active, false, Some(0.2));
        let late = track_background(&theme, Tone::Active, false, Some(0.7));
        assert_ne!(early, late);
        assert_eq!(
            track_background(&theme, Tone::Active, true, None),
            theme.palette().warning.into()
        );
        assert_eq!(
            track_background(&theme, Tone::Finished, false, Some(0.6)),
            theme.palette().success.into()
        );
        assert_eq!(
            track_background(&theme, Tone::Stopped, false, Some(0.6)),
            theme.palette().danger.into()
        );
    }

    #[test]
    fn drawer_opens_progressively_without_restarting_on_selection() {
        let mut presentation = Presentation::default();
        presentation.update(Message::ToggleDrawer, 2);
        let opened = presentation.drawer_opened_at().unwrap();
        assert_eq!(reveal(Some(opened), opened, false), 0.0);
        let middle = opened + REVEAL / 2;
        assert!((0.0..1.0).contains(&reveal(Some(opened), middle, false)));
        assert_eq!(reveal(Some(opened), middle, true), 1.0);
        assert_eq!(
            refresh_interval(&presentation, false, false, middle, false),
            Some(16)
        );
        assert_eq!(
            refresh_interval(&presentation, false, false, opened + REVEAL, false),
            None
        );
        assert_eq!(
            refresh_interval(&presentation, true, false, middle, true),
            Some(1000)
        );
        presentation.update(Message::SelectStep(2), 2);
        assert_eq!(presentation.drawer_opened_at(), Some(opened));
        presentation.update(Message::ToggleDrawer, 2);
        assert_eq!(presentation.drawer_opened_at(), None);
    }
}
