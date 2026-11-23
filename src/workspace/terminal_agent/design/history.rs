//! Native histories share the same bounded, redacted run activity.

use std::collections::VecDeque;

use iced::widget::{button, column, container, responsive, row, scrollable, text};
use iced::{Element, Length, window};

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
    pub(super) started: Option<std::time::Instant>,
    pub(super) live_run: Option<&'a Run>,
    pub(super) surface: Surface,
    pub(super) owner: Option<(&'a State, window::Id)>,
    pub(super) panel_review: Option<super::command_review::Review>,
}

impl<'a> From<&'a Run> for Activity<'a> {
    fn from(run: &'a Run) -> Self {
        let (transcript, started) = run.status_transcript().unzip();
        Self {
            activity: &run.activity,
            goal: &run.goal,
            plan: run.plan.as_deref(),
            activity_font: run.fonts.activity,
            plan_font: run.fonts.plan,
            presentation: &run.presentation,
            transcript,
            started,
            live_run: Some(run),
            surface: Surface::Activity,
            owner: None,
            panel_review: None,
        }
    }
}

impl<'a> Activity<'a> {
    pub(super) fn bound_step(
        self,
        index: usize,
    ) -> Option<&'a crate::terminal_agent::design::inline::Step> {
        let run = self.live_run?;
        let id = run.activity_steps.get(&run.presentation.identity(index))?;
        self.transcript?.steps.iter().find(|step| step.id == *id)
    }

    pub(super) fn step_context(
        self,
        index: usize,
        surface: Surface,
        content: Element<'a, AgentMessage>,
    ) -> Element<'a, AgentMessage> {
        let Some(run) = self.live_run else {
            return content;
        };
        let Some(step) = self.bound_step(index) else {
            return content;
        };
        super::log_table::step_context(content, run, step, surface)
    }

    pub(super) fn entry_menu(
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

pub(super) fn select(run: Activity<'_>, index: usize) -> AgentMessage {
    AgentMessage::Design(Message::Select(run.presentation.identity(index)))
}

pub(super) fn detail(run: Activity<'_>, selected: usize) -> Element<'_, AgentMessage> {
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
    panel_review: Option<super::command_review::Review>,
) -> Element<'a, AgentMessage> {
    view_as(
        Activity {
            surface,
            owner,
            panel_review,
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
    let usage_limit = if history == History::LogTable
        && run
            .transcript
            .is_some_and(|trace| super::log_table::has_detail(run, trace))
    {
        (font_size * 1.5).min(104.0)
    } else {
        104.0
    };
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
                .height(Length::Fixed((size.height * 0.28).min(usage_limit)))
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
    if history == History::Conversation
        && let Some(trace) = run.transcript
    {
        return super::conversation::thread(run, trace, palette, reduced_motion);
    }
    if history == History::LogTable
        && let Some(transcript) = run.transcript
        && (!transcript.steps.is_empty()
            || !transcript.final_answer.is_empty()
            || super::log_table::has_approval(run))
    {
        return super::log_table::view(run, transcript, palette, reduced_motion);
    }
    if history == History::Timeline
        && let Some(trace) = run.transcript
        && (!trace.steps.is_empty() || !trace.final_answer.is_empty() || run.activity.is_empty())
    {
        return super::timeline::view(run, trace, palette, reduced_motion);
    }
    let size = run.activity_font;
    let selected = run.presentation.selection(run.activity.len());
    let approval = match history {
        History::Timeline => super::command_review::card(run),
        _ => None,
    };
    let mut entries = column![].spacing(7).width(Length::Fill);
    match history {
        History::Transcript
        | History::StepCards
        | History::InlineBlocks
        | History::Focus
        | History::Conversation => {
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
        History::PlanExecution => return super::plan_execution::view(run, reduced_motion),
        History::CardStack => return super::card_stack::view(run, reduced_motion),
        History::Tmux => {
            return super::tmux::view(run, palette, reduced_motion);
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
    let history_rows = scrollable(entries).height(Length::Fill);
    if let Some(approval) = approval {
        let approval = if history == History::Timeline {
            row![
                text("●").style(|theme: &iced::Theme| text::Style {
                    color: Some(theme.extended_palette().warning.base.color),
                }),
                approval
            ]
            .spacing(5)
            .into()
        } else {
            approval
        };
        return column![history_rows, approval]
            .height(Length::Fill)
            .spacing(5)
            .into();
    }
    history_rows.into()
}
