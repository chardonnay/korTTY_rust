//! Native finished-run handoff controls. Opening and sending stay in the owned chat workflow.

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Element, Length};

use super::super::{Message, Phase, Run};

pub(in crate::workspace::terminal_agent) fn bar(run: &Run) -> Option<Element<'_, Message>> {
    if !run.can_chat_handoff() || run.chat_handoff_dismissed && !run.chat_handoff_pending {
        return None;
    }
    let status = match run.phase {
        Phase::Finished => "Run finished",
        Phase::PlanReady => "Plan ready",
        Phase::PlanHandedOff => "Plan accepted",
        _ => "Run stopped",
    };
    let heading = run
        .status_transcript()
        .map(|(trace, _)| trace.goal.as_str())
        .filter(|goal| !goal.trim().is_empty())
        .map_or_else(
            || status.to_owned(),
            |goal| format!("{status} · {}", super::history::summary(goal)),
        );
    Some(offer(
        &run.chat_follow_up,
        run.epoch,
        run.chat_handoff_enabled,
        run.chat_handoff_pending,
        heading,
    ))
}

pub(super) fn terminal_bar(run: &Run) -> Option<Element<'_, Message>> {
    if !run.is_terminal_command_run() {
        return None;
    }
    bar(run)
}

fn offer<'a>(
    question: &'a str,
    epoch: u64,
    available: bool,
    pending: bool,
    heading: String,
) -> Element<'a, Message> {
    let enabled = available && !pending;
    let input = text_input("Follow-up question about the run…", question)
        .id("agent-chat-follow-up")
        .on_input_maybe(enabled.then_some(move |value| Message::ChatFollowUp(epoch, value)))
        .on_submit_maybe(enabled.then_some(Message::ContinueInChat(epoch)))
        .size(13);
    let controls = row![
        input,
        continue_button(epoch, available, pending),
        container(
            button(text("Dismiss").size(13))
                .on_press_maybe((!pending).then_some(Message::DismissChatHandoff(epoch)))
        )
        .id("agent-chat-dismiss"),
    ]
    .spacing(6)
    .wrap();
    container(column![text(heading).size(12), controls].spacing(4))
        .padding(6)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
}

fn continue_button(epoch: u64, available: bool, pending: bool) -> Element<'static, Message> {
    container(
        button(
            text(if pending {
                "Opening chat…"
            } else {
                "Continue in AI Chat"
            })
            .size(13),
        )
        .on_press_maybe((available && !pending).then_some(Message::ContinueInChat(epoch))),
    )
    .id("agent-chat-continue")
    .into()
}

pub(in crate::workspace::terminal_agent) fn compact(run: &Run) -> Option<Element<'_, Message>> {
    run.can_chat_handoff().then(|| {
        continue_button(
            run.epoch,
            run.chat_handoff_enabled,
            run.chat_handoff_pending,
        )
    })
}

#[cfg(test)]
mod tests;
