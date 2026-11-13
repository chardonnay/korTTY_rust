use std::time::SystemTime;

use iced::widget::{button, column, container, row, rule, scrollable, text};
use iced::{Element, Font, Length};

use super::{Message, SchedulerView, SchedulerWindow};
use crate::scheduler_window::{
    RunProposalView,
    agent::{checked_proposal, invocation},
};

pub(super) fn status(proposals: &[RunProposalView<'_>]) -> &'static str {
    proposals
        .iter()
        .map(|proposal| match proposal {
            RunProposalView::Awaiting { .. } => (4, "Awaiting"),
            RunProposalView::Authorized(target) | RunProposalView::Executing(target)
                if target.initial_probe =>
            {
                (2, "Probing")
            }
            RunProposalView::Authorized(_) => (3, "Approved"),
            RunProposalView::Generating(_) => (1, "Generating"),
            RunProposalView::Executing(_) => (0, "Running"),
        })
        .max_by_key(|(priority, _)| *priority)
        .map_or("Running", |(_, label)| label)
}

pub(super) fn view<'a>(
    runtime: &SchedulerWindow,
    scheduler_view: &SchedulerView,
    job_name: &'a str,
    proposals: Vec<RunProposalView<'a>>,
) -> Element<'a, Message> {
    let mut details = column![text(job_name).size(19)].spacing(8);
    for proposal in proposals {
        details = details.push(
            container(target_view(runtime, scheduler_view, proposal))
                .style(container::bordered_box)
                .width(Length::Fill)
                .padding(8),
        );
    }
    scrollable(details).height(Length::Fill).into()
}

fn target_view<'a>(
    runtime: &SchedulerWindow,
    scheduler_view: &SchedulerView,
    proposal: RunProposalView<'a>,
) -> Element<'a, Message> {
    let target = proposal.target();
    let mut details = column![
        text(format!(
            "Target {} of {} · {}",
            target.index + 1,
            target.count,
            target.route.reference.target.label()
        ))
        .wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(6);
    if let Some(round) = target.swarm_round {
        details = details.push(text(format!("Swarm round {round}")).size(13));
    }
    let mut actions = None;
    match proposal {
        RunProposalView::Generating(_) => {
            details = details.push(text("Generating the AI Agent proposal for this target…"));
        }
        RunProposalView::Authorized(_) => {
            details = details.push(text(if target.initial_probe {
                "The saved environment probe is queued for this target."
            } else {
                "The proposal was approved for this target."
            }));
        }
        RunProposalView::Executing(_) => {
            details = details.push(text(if target.initial_probe {
                "Running the reviewed environment probe for this target…"
            } else {
                "Executing the approved commands for this target…"
            }));
        }
        RunProposalView::Awaiting {
            items,
            directory,
            expires_at,
            ticket,
            ..
        } => {
            let expired = SystemTime::now() >= expires_at;
            let can_decide = runtime.can_decide_proposal(scheduler_view, ticket);
            details = details
                .push(text("Review AI Agent proposal").size(16))
                .push(text(format!("Run {}", ticket.run())).size(12))
                .push(
                    text(format!(
                        "{} {}",
                        if expired {
                            "Review expired"
                        } else {
                            "Review expires"
                        },
                        runtime.local_time(expires_at)
                    ))
                    .size(12),
                )
                .push(text(format!("Working directory · {directory}")))
                .push(text(if target.swarm_round.is_some() {
                    "Approval authorizes these exact commands for the displayed target and Swarm round."
                } else {
                    "Approval authorizes these exact commands on the displayed SSH target for this run."
                }).size(13));
            if let Some(jump) = &target.route.reference.jump {
                details = details.push(text(format!("Via {}", jump.label())).size(12));
            }
            for pin in &target.route.pins {
                details = details.push(text(pin).size(12));
            }
            for (index, item) in items.iter().enumerate() {
                let checked = checked_proposal(&item.command, directory);
                details = details
                    .push(rule::horizontal(1))
                    .push(
                        text(format!(
                            "{}. Target {} · {}",
                            index + 1,
                            item.target,
                            item.purpose
                        ))
                        .size(13),
                    )
                    .push(text("Proposed command").size(12))
                    .push(
                        text(&item.command)
                            .font(Font::MONOSPACE)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .push(text("SSH invocation").size(12))
                    .push(
                        text(invocation(directory, &checked))
                            .font(Font::MONOSPACE)
                            .size(12)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    );
            }
            actions = Some(
                row![
                    button(if target.count == 1 {
                        "Approve this run"
                    } else {
                        "Approve target commands"
                    })
                    .on_press_maybe(
                        can_decide.then(|| Message::ApproveRunProposal(ticket.clone()))
                    ),
                    button(if target.count == 1 {
                        "Reject this run"
                    } else {
                        "Reject target commands"
                    })
                    .style(button::secondary)
                    .on_press_maybe(can_decide.then(|| Message::RejectRunProposal(ticket.clone()))),
                ]
                .spacing(8)
                .wrap(),
            );
        }
    }
    if let Some(actions) = actions {
        details = details.push(actions);
    }
    details.into()
}
