//! Native histories share the same bounded, redacted run activity.

use iced::widget::{button, column, container, responsive, row, scrollable, text};
use iced::{Element, Font, Length};

use crate::terminal_agent::design::History;

use super::super::Message as AgentMessage;
use super::{Message, Palette, Run};

pub(super) fn summary(value: &str) -> String {
    let line = value
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Activity");
    let mut result: String = line.chars().take(72).collect();
    if line.chars().count() > 72 {
        result.push('…');
    }
    result
}

fn select(run: &Run, index: usize) -> AgentMessage {
    AgentMessage::Design(Message::Select(run.presentation.identity(index)))
}

fn detail(run: &Run, selected: usize) -> Element<'_, AgentMessage> {
    text(
        run.activity
            .get(selected)
            .map_or("No activity yet.", String::as_str),
    )
    .width(Length::Fill)
    .into()
}

pub(super) fn view(run: &Run, palette: Option<Palette>) -> Element<'_, AgentMessage> {
    view_as(run, run.design.history, palette)
}

pub(super) fn view_as(
    run: &Run,
    history: History,
    palette: Option<Palette>,
) -> Element<'_, AgentMessage> {
    if run.presentation.model_usage.label().is_none() {
        return entries(run, history);
    }
    let muted = palette.map(|palette| palette.muted.color());
    responsive(move |size| {
        let mut usage = column![
            text(run.presentation.model_usage.label().unwrap_or_default())
                .size(12)
                .style(move |_| text::Style { color: muted })
        ]
        .spacing(4);
        for row in run.presentation.model_usage.rows() {
            usage = usage.push(
                text(row)
                    .size(12)
                    .style(move |_| text::Style { color: muted }),
            );
        }
        // Keep command history usable in a short bottom dock or picture-in-picture panel.
        column![
            scrollable(usage)
                .width(Length::Fill)
                .height(Length::Fixed((size.height * 0.28).min(104.0)))
                .spacing(6),
            entries(run, history),
        ]
        .spacing((size.height * 0.03).min(6.0))
        .height(Length::Fill)
        .into()
    })
    .into()
}

fn entries(run: &Run, history: History) -> Element<'_, AgentMessage> {
    let selected = run.presentation.selection(run.activity.len());
    let mut entries = column![].spacing(7).width(Length::Fill);
    match history {
        History::Transcript | History::StepCards | History::InlineBlocks | History::Focus => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(super::activity_entry(history, index, entry));
            }
        }
        History::Timeline => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    row![
                        text(if index == selected { "●" } else { "│" }),
                        column![
                            button(text(format!("{} · {}", index + 1, summary(entry))).size(13))
                                .on_press(select(run, index))
                                .style(button::text),
                            (index == selected).then(|| text(entry).size(13)),
                        ]
                        .spacing(4)
                        .width(Length::Fill),
                    ]
                    .spacing(5),
                );
            }
        }
        History::PlanExecution => {
            let mut steps = column![text("Goal").size(14), text(&run.goal).size(13)]
                .spacing(5)
                .width(Length::FillPortion(2));
            if let Some(plan) = &run.plan {
                steps = steps.push(text(plan).size(13));
            }
            steps = steps.push(text("Activity").size(14));
            for (index, entry) in run.activity.iter().enumerate() {
                steps = steps.push(
                    button(text(format!("{}  {}", index + 1, summary(entry))).size(12))
                        .on_press(select(run, index))
                        .style(if index == selected {
                            button::primary
                        } else {
                            button::text
                        }),
                );
            }
            return row![
                scrollable(steps)
                    .height(Length::Fill)
                    .width(Length::FillPortion(2)),
                scrollable(
                    column![text("Selected activity").size(14), detail(run, selected)].spacing(7)
                )
                .height(Length::Fill)
                .width(Length::FillPortion(3)),
            ]
            .spacing(10)
            .into();
        }
        History::CardStack => {
            for index in selected.saturating_sub(2)..selected {
                if let Some(entry) = run.activity.get(index) {
                    entries = entries.push(
                        button(text(format!("{} · {}", index + 1, summary(entry))).size(12))
                            .width(Length::Fill)
                            .on_press(select(run, index))
                            .style(button::secondary),
                    );
                }
            }
            entries = entries.push(
                container(
                    column![
                        row![
                            button("Previous").on_press_maybe(
                                (selected > 0).then_some(AgentMessage::Design(Message::Previous))
                            ),
                            text(format!(
                                "Activity {} of {}",
                                if run.activity.is_empty() {
                                    0
                                } else {
                                    selected + 1
                                },
                                run.activity.len()
                            )),
                            button("Next").on_press_maybe(
                                (selected + 1 < run.activity.len())
                                    .then_some(AgentMessage::Design(Message::Next))
                            ),
                        ]
                        .spacing(6)
                        .wrap(),
                        detail(run, selected),
                    ]
                    .spacing(8),
                )
                .padding(10)
                .style(container::bordered_box)
                .width(Length::Fill),
            );
        }
        History::Tmux => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    column![
                        text(format!("── activity {} ──", index + 1))
                            .font(Font::MONOSPACE)
                            .size(12),
                        text(entry).font(Font::MONOSPACE).size(13),
                    ]
                    .spacing(4),
                );
            }
            return container(scrollable(entries).height(Length::Fill))
                .padding(8)
                .style(container::bordered_box)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        History::Conversation => {
            entries = entries.push(
                container(
                    container(text(&run.goal).size(13))
                        .padding(8)
                        .style(container::rounded_box)
                        .max_width(360),
                )
                .width(Length::Fill)
                .align_right(Length::Fill),
            );
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    container(
                        column![
                            button(text(format!("Agent · {}", summary(entry))).size(13))
                                .on_press(select(run, index))
                                .style(button::text),
                            (index == selected).then(|| text(entry).size(13)),
                        ]
                        .spacing(4),
                    )
                    .padding(7)
                    .style(container::bordered_box)
                    .width(Length::Fill),
                );
            }
        }
        History::LogTable => {
            entries = entries
                .push(row![text("#").width(36), text("Activity").width(Length::Fill)].spacing(6));
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    button(
                        row![
                            text((index + 1).to_string()).size(12).width(36),
                            text(summary(entry)).size(12).width(Length::Fill),
                        ]
                        .spacing(6),
                    )
                    .width(Length::Fill)
                    .on_press(select(run, index))
                    .style(if index == selected {
                        button::primary
                    } else {
                        button::text
                    }),
                );
            }
            entries = entries.push(
                container(detail(run, selected))
                    .padding(8)
                    .style(container::bordered_box)
                    .width(Length::Fill),
            );
        }
    }
    scrollable(entries).height(Length::Fill).into()
}
