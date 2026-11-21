//! Native histories share the same bounded, redacted run activity.

use std::collections::VecDeque;

use iced::widget::{button, column, container, responsive, row, scrollable, text};
use iced::{Element, Font, Length, window};

use crate::terminal_agent::design::History;

use super::super::Message as AgentMessage;
use super::log_table::Surface;
use super::{Message, Palette, Presentation, Run, State};

#[derive(Clone, Copy)]
pub(super) struct Activity<'a> {
    pub(super) activity: &'a VecDeque<String>,
    pub(super) goal: &'a str,
    pub(super) plan: Option<&'a str>,
    pub(super) activity_font: f32,
    pub(super) plan_font: f32,
    pub(super) presentation: &'a Presentation,
    pub(super) transcript: Option<&'a crate::terminal_agent::design::inline::Transcript>,
    pub(super) live_run: Option<&'a Run>,
    pub(super) surface: Surface,
    pub(super) owner: Option<(&'a State, window::Id)>,
}

impl<'a> From<&'a Run> for Activity<'a> {
    fn from(run: &'a Run) -> Self {
        Self {
            activity: &run.activity,
            goal: &run.goal,
            plan: run.plan.as_deref(),
            activity_font: run.fonts.activity,
            plan_font: run.fonts.plan,
            presentation: &run.presentation,
            transcript: run.status_transcript().map(|(trace, _)| trace),
            live_run: Some(run),
            surface: Surface::Activity,
            owner: None,
        }
    }
}

impl<'a> Activity<'a> {
    pub(super) fn step_context(
        self,
        index: usize,
        surface: Surface,
        content: Element<'a, AgentMessage>,
    ) -> Element<'a, AgentMessage> {
        let Some(run) = self.live_run else {
            return content;
        };
        let Some(step) = run
            .activity_steps
            .get(&run.presentation.identity(index))
            .and_then(|id| self.transcript?.steps.iter().find(|step| step.id == *id))
        else {
            return content;
        };
        super::log_table::step_context(content, run, step, surface)
    }

    fn entry_menu(
        self,
        index: usize,
        content: impl Into<Element<'a, AgentMessage>>,
    ) -> Element<'a, AgentMessage> {
        let surface = self
            .surface
            .activity_entry(self.presentation.identity(index));
        self.step_context(index, surface, content.into())
    }
}

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

fn select(run: Activity<'_>, index: usize) -> AgentMessage {
    AgentMessage::Design(Message::Select(run.presentation.identity(index)))
}

fn detail(run: Activity<'_>, selected: usize) -> Element<'_, AgentMessage> {
    if run.presentation.activity.expand_all && !run.activity.is_empty() {
        return run
            .activity
            .iter()
            .fold(column![].spacing(8), |entries, entry| {
                entries.push(text(entry).size(run.activity_font).width(Length::Fill))
            })
            .into();
    }
    text(
        run.activity
            .get(selected)
            .map_or("No activity yet.", String::as_str),
    )
    .size(run.activity_font)
    .width(Length::Fill)
    .into()
}

pub(super) fn view<'a>(
    run: &'a Run,
    palette: Option<Palette>,
    reduced_motion: bool,
    surface: Surface,
    owner: Option<(&'a State, window::Id)>,
) -> Element<'a, AgentMessage> {
    view_as(
        Activity {
            surface,
            owner,
            ..run.into()
        },
        run.design.history,
        palette,
        reduced_motion,
    )
}

pub(super) fn view_as(
    run: Activity<'_>,
    history: History,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'_, AgentMessage> {
    if run.presentation.model_usage.label().is_none() {
        return entries(run, history, palette, reduced_motion);
    }
    with_usage(run, history, palette, reduced_motion)
}

fn with_usage(
    run: Activity<'_>,
    history: History,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'_, AgentMessage> {
    let font_size = run.activity_font;
    let muted = palette.map(|palette| palette.muted.color());
    responsive(move |size| {
        let mut usage = column![
            text(run.presentation.model_usage.label().unwrap_or_default())
                .size(font_size - 1.0)
                .style(move |_| text::Style { color: muted })
        ]
        .spacing(4);
        for row in run.presentation.model_usage.rows() {
            usage = usage.push(
                text(row)
                    .size(font_size - 1.0)
                    .style(move |_| text::Style { color: muted }),
            );
        }
        // Keep command history usable in a short bottom dock or picture-in-picture panel.
        column![
            scrollable(usage)
                .width(Length::Fill)
                .height(Length::Fixed((size.height * 0.28).min(104.0)))
                .spacing(6),
            entries(run, history, palette, reduced_motion),
        ]
        .spacing((size.height * 0.03).min(6.0))
        .height(Length::Fill)
        .into()
    })
    .into()
}

fn entries(
    run: Activity<'_>,
    history: History,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'_, AgentMessage> {
    if history == History::LogTable
        && let Some(transcript) = run.transcript
        && (!transcript.steps.is_empty()
            || !transcript.final_answer.is_empty()
            || super::log_table::has_approval(run))
    {
        return super::log_table::view(run, transcript, palette, reduced_motion);
    }
    let size = run.activity_font;
    let selected = run.presentation.selection(run.activity.len());
    let mut entries = column![].spacing(7).width(Length::Fill);
    match history {
        History::Transcript | History::StepCards | History::InlineBlocks | History::Focus => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    run.entry_menu(index, super::activity_entry(history, index, entry, size)),
                );
            }
        }
        History::Timeline => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    run.entry_menu(
                        index,
                        row![
                            text(if index == selected { "●" } else { "│" }),
                            column![
                                button(
                                    text(format!("{} · {}", index + 1, summary(entry))).size(size)
                                )
                                .on_press(select(run, index))
                                .style(button::text),
                                (index == selected || run.presentation.activity.expand_all)
                                    .then(|| text(entry).size(size)),
                            ]
                            .spacing(4)
                            .width(Length::Fill),
                        ]
                        .spacing(5),
                    ),
                );
            }
        }
        History::PlanExecution => {
            let mut steps = column![text("Goal").size(size + 1.0), text(run.goal).size(size)]
                .spacing(5)
                .width(Length::FillPortion(2));
            if let Some(plan) = run.plan {
                steps = steps.push(text(plan).size(run.plan_font));
            }
            steps = steps.push(text("Activity").size(size + 1.0));
            for (index, entry) in run.activity.iter().enumerate() {
                steps = steps.push(
                    run.entry_menu(
                        index,
                        button(text(format!("{}  {}", index + 1, summary(entry))).size(size - 1.0))
                            .on_press(select(run, index))
                            .style(if index == selected {
                                button::primary
                            } else {
                                button::text
                            }),
                    ),
                );
            }
            return row![
                scrollable(steps)
                    .height(Length::Fill)
                    .width(Length::FillPortion(2)),
                scrollable(
                    column![
                        text("Selected activity").size(size + 1.0),
                        detail(run, selected)
                    ]
                    .spacing(7)
                )
                .height(Length::Fill)
                .width(Length::FillPortion(3)),
            ]
            .spacing(10)
            .into();
        }
        History::CardStack if run.presentation.activity.expand_all => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    run.entry_menu(index, super::activity_entry(history, index, entry, size)),
                );
            }
        }
        History::CardStack => {
            for index in selected.saturating_sub(2)..selected {
                if let Some(entry) = run.activity.get(index) {
                    entries = entries.push(
                        run.entry_menu(
                            index,
                            button(
                                text(format!("{} · {}", index + 1, summary(entry)))
                                    .size(size - 1.0),
                            )
                            .width(Length::Fill)
                            .on_press(select(run, index))
                            .style(button::secondary),
                        ),
                    );
                }
            }
            entries = entries.push(
                run.entry_menu(
                    selected,
                    container(
                        column![
                            row![
                                button("Previous").on_press_maybe(
                                    (selected > 0)
                                        .then_some(AgentMessage::Design(Message::Previous))
                                ),
                                text(format!(
                                    "Activity {} of {}",
                                    if run.activity.is_empty() {
                                        0
                                    } else {
                                        selected + 1
                                    },
                                    run.activity.len()
                                ))
                                .size(size),
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
                ),
            );
        }
        History::Tmux => {
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    run.entry_menu(
                        index,
                        column![
                            text(format!("── activity {} ──", index + 1))
                                .font(Font::MONOSPACE)
                                .size(size - 1.0),
                            text(entry).font(Font::MONOSPACE).size(size),
                        ]
                        .spacing(4),
                    ),
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
                    container(text(run.goal).size(size))
                        .padding(8)
                        .style(container::rounded_box)
                        .max_width(360),
                )
                .width(Length::Fill)
                .align_right(Length::Fill),
            );
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    run.entry_menu(
                        index,
                        container(
                            column![
                                button(text(format!("Agent · {}", summary(entry))).size(size))
                                    .on_press(select(run, index))
                                    .style(button::text),
                                (index == selected || run.presentation.activity.expand_all)
                                    .then(|| text(entry).size(size)),
                            ]
                            .spacing(4),
                        )
                        .padding(7)
                        .style(container::bordered_box)
                        .width(Length::Fill),
                    ),
                );
            }
        }
        History::LogTable => {
            entries = entries.push(
                row![
                    text("#").size(size).width(36),
                    text("Activity").size(size).width(Length::Fill)
                ]
                .spacing(6),
            );
            for (index, entry) in run.activity.iter().enumerate() {
                entries = entries.push(
                    button(
                        row![
                            text((index + 1).to_string()).size(size - 1.0).width(36),
                            text(summary(entry)).size(size - 1.0).width(Length::Fill),
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
