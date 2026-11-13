//! Status modules expose existing run actions without creating approvals.

use iced::widget::{button, column, container, row, scrollable, text, tooltip};
use iced::{Alignment, Element, Font, Length};

use crate::terminal_agent::design::Status;

use super::super::{Message as AgentMessage, Phase};
use super::{
    Message, Run, history,
    palette::{self, Palette},
};

pub(super) fn phase(run: &Run) -> (&'static str, usize) {
    if run.cancel.requested() && run.phase.working() {
        return ("Cancelling", 2);
    }
    if run.paused && !run.phase.finished() {
        return ("Paused", 2);
    }
    if run.phase.deciding() && run.presentation.model_usage.repairing(run.epoch) {
        return ("Repairing response", 2);
    }
    match run.phase {
        Phase::Setup | Phase::Directory | Phase::SavedPreflight => ("Setup", 0),
        Phase::ModelReview => (
            run.model_review
                .as_ref()
                .map_or("Review model request", |review| review.title()),
            1,
        ),
        Phase::DirectoryConfirmation => ("Confirm directory", 1),
        Phase::ReviewedDirectory { .. }
        | Phase::ProbeApproval
        | Phase::Approval
        | Phase::Question => ("Review", 1),
        Phase::SudoPassword => ("Waiting for sudo password", 1),
        Phase::PlanQuestions | Phase::PlanOptions | Phase::PlanReady | Phase::PlanLibrary => {
            ("Planning review", 1)
        }
        Phase::FinalAdvice => ("Preparing final response", 2),
        Phase::Finished => ("Completed", 3),
        Phase::Failed => ("Failed", 3),
        Phase::Blocked => ("Blocked", 3),
        Phase::Cancelled => ("Cancelled", 3),
        _ => ("Working", 2),
    }
}

fn station(
    run: &Run,
    index: usize,
    vertical: bool,
    palette: Option<Palette>,
) -> Element<'_, AgentMessage> {
    let selected = run.presentation.selection(run.activity.len()) == index;
    let marker = if selected { "●" } else { "○" };
    let color = palette.map(|palette| {
        if selected {
            palette.accent.color()
        } else {
            palette.muted.color()
        }
    });
    tooltip(
        button(
            text(if vertical {
                format!("{marker} {}", index + 1)
            } else {
                marker.to_owned()
            })
            .size(13)
            .style(move |_| text::Style { color }),
        )
        .style(button::text)
        .on_press(AgentMessage::Design(Message::Select(
            run.presentation.identity(index),
        ))),
        container(
            text(
                run.activity
                    .get(index)
                    .map_or("No activity yet.", String::as_str),
            )
            .size(12),
        )
        .padding(7)
        .max_width(420)
        .style(container::bordered_box),
        if vertical {
            tooltip::Position::Left
        } else {
            tooltip::Position::Top
        },
    )
    .into()
}

pub(super) fn view(
    run: &Run,
    status: Status,
    palette: Option<Palette>,
) -> Element<'_, AgentMessage> {
    let (phase, stage) = phase(run);
    let accent = palette.map(|palette| palette.accent.color());
    let heading_color = if matches!(run.phase, Phase::Failed | Phase::Blocked) {
        palette.map(|palette| palette.error.color())
    } else {
        None
    };
    let heading_style = move |_: &iced::Theme| text::Style {
        color: heading_color,
    };
    let toggle = || AgentMessage::Design(Message::ToggleDrawer);
    let header: Element<'_, AgentMessage> = match status {
        Status::Hidden => return column![].into(),
        Status::Steps | Status::Pill => row![
            super::status_indicator_with_colors(status, phase, stage, heading_color, accent),
            button("Agent activity")
                .style(button::text)
                .on_press(AgentMessage::Focus),
        ]
        .spacing(7)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::Metro => {
            let mut stations = row![
                text(format!("Agent · {phase}"))
                    .size(12)
                    .style(heading_style)
            ]
            .spacing(3)
            .align_y(Alignment::Center);
            let first = run.activity.len().saturating_sub(8);
            if first > 0 {
                stations = stations.push(
                    button(text(format!("+{first}")))
                        .style(button::text)
                        .on_press(AgentMessage::Design(Message::Select(
                            run.presentation.identity(first - 1),
                        ))),
                );
            }
            for index in first..run.activity.len() {
                stations = stations.push(station(run, index, false, palette));
            }
            stations
                .push(button("Details").style(button::text).on_press(toggle()))
                .wrap()
                .into()
        }
        Status::Minimap => {
            let mut rail = column![text(phase).size(11).style(heading_style)].spacing(2);
            if let Some(usage) = usage_label(run, palette) {
                rail = rail.push(usage);
            }
            for index in 0..run.activity.len() {
                rail = rail.push(station(run, index, true, palette));
            }
            let rail = column![
                scrollable(rail).height(Length::Fill),
                button("Activity")
                    .style(button::text)
                    .on_press(AgentMessage::Focus),
            ]
            .spacing(3);
            return if palette.is_some() {
                palette::panel(palette, container(rail))
            } else {
                rail.into()
            };
        }
        Status::Powerline => row![
            container(text(" AGENT ").size(12))
                .padding(4)
                .style(container::rounded_box),
            text("❯").size(15),
            text(phase).size(12).style(heading_style),
            text("❯").size(15),
            text(format!("{} activities", run.activity.len())).size(12),
            button(if run.presentation.drawer {
                "Close details"
            } else {
                "Details"
            })
            .style(button::text)
            .on_press(toggle()),
        ]
        .spacing(5)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::TopBar => row![
            text(format!("Agent · {phase}"))
                .size(13)
                .style(heading_style),
            button(if run.presentation.drawer {
                "Close drawer"
            } else {
                "Open drawer"
            })
            .style(button::text)
            .on_press(toggle()),
            button("Activity window")
                .style(button::text)
                .on_press(AgentMessage::Focus),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::Neon => row![
            text(format!("[{}]", phase.to_uppercase()))
                .font(Font::MONOSPACE)
                .size(12)
                .style(heading_style),
            text(run.activity.back().map_or_else(
                || "Waiting for activity".into(),
                |entry| history::summary(entry)
            ))
            .font(Font::MONOSPACE)
            .size(12),
            button("Details").style(button::text).on_press(toggle()),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .wrap()
        .into(),
        Status::Hud => column![
            text(format!("Agent · {phase}"))
                .size(14)
                .style(heading_style),
            text(history::summary(&run.goal)).size(12),
            text(
                run.activity
                    .back()
                    .map_or_else(String::new, |entry| history::summary(entry))
            )
            .size(12),
            button("Show activity")
                .style(button::text)
                .on_press(toggle()),
        ]
        .spacing(5)
        .into(),
        Status::Spotlight => button(
            text(format!("● Agent · {phase}"))
                .size(13)
                .style(heading_style),
        )
        .style(button::secondary)
        .on_press(toggle())
        .into(),
        Status::Toasts => {
            let mut cards = column![
                text(format!("Agent · {phase}"))
                    .size(12)
                    .style(heading_style)
            ]
            .spacing(5);
            for (index, entry) in run
                .activity
                .iter()
                .enumerate()
                .skip(run.activity.len().saturating_sub(3))
            {
                cards = cards.push(
                    button(text(history::summary(entry)).size(12))
                        .style(button::secondary)
                        .width(Length::Fill)
                        .on_press(AgentMessage::Design(Message::Select(
                            run.presentation.identity(index),
                        ))),
                );
            }
            cards
                .push(
                    button("Activity window")
                        .style(button::text)
                        .on_press(AgentMessage::Focus),
                )
                .into()
        }
        Status::Orbit => column![
            text("○  ●  ○").size(13),
            button(
                text(format!("Agent\n{phase}\n{} activities", run.activity.len()))
                    .size(12)
                    .style(heading_style)
            )
            .style(button::secondary)
            .on_press(toggle()),
            text("○  ○  ○").size(13),
        ]
        .align_x(Alignment::Center)
        .spacing(2)
        .into(),
    };
    let drawer = run.presentation.drawer;
    let simple = matches!(status, Status::Steps | Status::Pill);
    let mut panel = column![header, usage_label(run, palette)].spacing(5);
    if drawer && !simple {
        let selected = run.presentation.selection(run.activity.len());
        panel = panel.push(
            scrollable(
                text(
                    run.activity
                        .get(selected)
                        .map_or("No activity yet.", String::as_str),
                )
                .size(13),
            )
            .height(140),
        );
        panel = panel.push(super::controls(run));
    }
    palette::panel(palette, container(panel).padding(5))
}

fn usage_label(run: &Run, palette: Option<Palette>) -> Option<Element<'_, AgentMessage>> {
    let compact = run.presentation.model_usage.compact()?;
    let label = run.presentation.model_usage.label()?;
    let muted = palette.map(|palette| palette.muted.color());
    Some(
        tooltip(
            text(compact)
                .size(11)
                .style(move |_| text::Style { color: muted }),
            container(text(label).size(12)).padding(5).max_width(420),
            tooltip::Position::Bottom,
        )
        .into(),
    )
}
