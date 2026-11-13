//! Optional comparison approval for a standing Swarm review.

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length, Task};

use crate::local_ai;
use crate::terminal_agent::swarm::comparison::{self, ScheduledTemplate};
use crate::workspace::ai_quota;

use super::{Message, ProfileChoice, ProposalAuthority, SchedulerView, SchedulerWindow};

pub(in crate::scheduler_window) struct Review {
    ticket: String,
    template: ScheduledTemplate,
    quota: ai_quota::State,
    cli: Option<(String, local_ai::cli::Request)>,
}

impl Review {
    fn new(template: ScheduledTemplate, profile: &local_ai::Profile) -> Result<Self, String> {
        let [system, input] = template.messages();
        let cli = profile
            .cli_command_review()
            .map(|command| {
                profile
                    .prepare_cli_chat(system, &[], input)
                    .map(|request| (command, request))
                    .map_err(|error| error.message())
            })
            .transpose()?;
        Ok(Self {
            ticket: nonce()?,
            template,
            quota: ai_quota::State::default(),
            cli,
        })
    }

    fn forecast(&mut self, profile: &local_ai::Profile) -> Task<Message> {
        let [system, _] = self.template.messages();
        let input = self.template.forecast_input();
        let ticket = self.ticket.clone();
        self.quota.start_messages(
            0,
            &comparison::transport(profile),
            [system, input.as_str()],
            move |forecast| Message::SwarmComparisonQuotaFinished(ticket, forecast),
        )
    }

    pub(in crate::scheduler_window) fn view<'a>(
        &'a self,
        profile: &'a ProfileChoice,
    ) -> Element<'a, Message> {
        let [system, input] = self.template.messages();
        let mut details = column![
            text("Review scheduled Swarm comparison").size(17),
            text(format!("AI profile · {profile}")),
            text(format!("AI destination · {}", profile.destination)).size(12),
            text("After the target reports are complete, the model compares the masked results in the reviewed order.").size(13),
            text(format!(
                "The status and result placeholders are filled for each target. The forecast allows up to {} JSON-encoded result characters per target.",
                self.template.detail_limit(),
            )).size(13),
            self.quota.label().map(|forecast| text(forecast).size(13)),
        ].spacing(5);
        if let Some((command, request)) = &self.cli {
            details = details
                .push(
                    text(command)
                        .size(12)
                        .font(iced::Font::MONOSPACE)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .push(super::super::view::reviewed_cli_payload(request));
        } else {
            details = details
                .push(text("System instructions").size(13))
                .push(text(system).size(12).wrapping(text::Wrapping::WordOrGlyph))
                .push(text("Masked request template").size(13))
                .push(
                    text(input)
                        .font(iced::Font::MONOSPACE)
                        .wrapping(text::Wrapping::WordOrGlyph),
                );
        }
        column![
            scrollable(details).height(Length::Fixed(190.0)),
            row![
                button("Back to Swarm review")
                    .on_press(Message::CancelSwarmComparison(self.ticket.clone())),
                button("Approve scheduled comparison").on_press_maybe(
                    (!self.quota.loading())
                        .then(|| Message::ApproveSwarmComparison(self.ticket.clone())),
                ),
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(6)
        .into()
    }
}

impl super::Review {
    fn close_comparison_preview(&mut self) -> Result<(), String> {
        if let Some(skills) = self.model.as_mut().and_then(|model| model.skills.as_mut()) {
            skills.renew_preview()?;
        } else {
            self.generation_ticket = Some(nonce()?);
        }
        self.comparison_review = None;
        Ok(())
    }
}

impl SchedulerWindow {
    pub(crate) fn swarm_comparison_review_authority<'a>(
        &self,
        view: &'a SchedulerView,
        ticket: &str,
        approving: bool,
    ) -> Result<Option<ProposalAuthority<'a>>, String> {
        let Some(review) = view
            .agent_review
            .as_ref()
            .filter(|review| review.supports_comparison())
        else {
            return Ok(None);
        };
        let current = if approving {
            review
                .comparison_review
                .as_ref()
                .is_some_and(|preview| preview.ticket == ticket && !preview.quota.loading())
        } else {
            review.approval.comparison.is_none() && review.firing_review_ticket() == Some(ticket)
        };
        if !current {
            return Ok(None);
        }
        self.proposal_authority(view)
    }

    pub(crate) fn finish_swarm_comparison_review(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        template: ScheduledTemplate,
    ) -> Result<Task<Message>, String> {
        let Some(authority) = self.swarm_comparison_review_authority(view, ticket, false)? else {
            return Ok(Task::none());
        };
        if authority.prompt.profile_revision() != Some(template.grant().profile_revision()) {
            return Err("The scheduled Swarm comparison profile changed. Review again.".into());
        }
        let mut preview = Review::new(template, authority.transport)?;
        let task = preview.forecast(authority.transport);
        view.agent_review
            .as_mut()
            .expect("current Swarm review")
            .comparison_review = Some(preview);
        Ok(task)
    }

    pub(crate) fn approve_swarm_comparison(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
        template: &ScheduledTemplate,
    ) -> Result<Task<Message>, String> {
        let Some(authority) = self.swarm_comparison_review_authority(view, ticket, true)? else {
            return Ok(Task::none());
        };
        let profile = authority.transport.clone();
        let review = view.agent_review.as_mut().expect("current Swarm review");
        let preview = review
            .comparison_review
            .as_mut()
            .expect("current comparison preview");
        let grant = preview.template.grant();
        if !grant.matches(template) {
            return Err("The scheduled Swarm comparison request changed. Review it again.".into());
        }
        if preview.quota.stale() {
            preview.ticket = nonce()?;
            view.status = Some("Review the refreshed comparison quota before approving.".into());
            return Ok(preview.forecast(&profile));
        }
        review.close_comparison_preview()?;
        review.approval.comparison = Some(grant);
        view.status = Some("Comparison approved. Approve the Swarm requests and save the job to keep both reviews.".into());
        Ok(Task::none())
    }

    pub(crate) fn update_swarm_comparison_review(
        &self,
        view: &mut SchedulerView,
        message: &Message,
    ) -> Option<Task<Message>> {
        match message {
            Message::CancelSwarmComparison(ticket) => {
                if let Err(error) = self.cancel_swarm_comparison(view, ticket) {
                    view.status = Some(error);
                }
            }
            Message::SwarmComparisonQuotaFinished(ticket, forecast) => {
                if let Some(preview) = view
                    .agent_review
                    .as_mut()
                    .and_then(|review| review.comparison_review.as_mut())
                    && preview.ticket == *ticket
                {
                    preview.quota.complete(0, forecast.clone());
                }
            }
            _ => return None,
        }
        Some(Task::none())
    }

    fn cancel_swarm_comparison(
        &self,
        view: &mut SchedulerView,
        ticket: &str,
    ) -> Result<(), String> {
        let Some(review) = view
            .agent_review
            .as_ref()
            .filter(|review| review.supports_comparison())
        else {
            return Ok(());
        };
        let current = match &review.comparison_review {
            Some(preview) => preview.ticket == ticket,
            None => {
                review.approval.comparison.is_some()
                    && review.firing_review_ticket() == Some(ticket)
            }
        };
        if !current {
            return Ok(());
        }
        self.agent_review_current(view, review)?;
        let review = view.agent_review.as_mut().expect("current Swarm review");
        review.close_comparison_preview()?;
        let removed = review.approval.comparison.take().is_some();
        view.status = Some(
            if removed {
                "Scheduled comparison approval removed. Review the Swarm requests."
            } else {
                "Review the scheduled Swarm requests."
            }
            .into(),
        );
        Ok(())
    }
}

fn nonce() -> Result<String, String> {
    crate::private_files::nonce()
        .map_err(|_| "Could not identify the Swarm comparison review.".into())
}

#[cfg(test)]
mod tests {
    use crate::scheduler_window::tests::Profile;
    use crate::terminal_agent::swarm::comparison::ScheduledScope;

    use super::super::Probe;
    use super::super::tests::{mask, proposal_fixture};
    use super::*;

    #[test]
    fn comparison_approval_keeps_the_primary_swarm_approval_separate() {
        let root = Profile::temporary();
        let (scheduler, mut view, _, transport, redactor) =
            proposal_fixture(&root, Probe::SwarmIterative);
        let template = |goal| {
            ScheduledTemplate::prepare(
                ScheduledScope {
                    profile: &transport,
                    profile_revision: [7; 32],
                    context_limit: 4096,
                    targets: &[1, 2],
                },
                goal,
                None,
                &redactor,
                mask,
            )
            .unwrap()
        };
        let reviewed = template("Inspect disk usage");
        let primary_ticket = view.firing_review_ticket().unwrap().to_owned();
        let _ = scheduler
            .finish_swarm_comparison_review(&mut view, &primary_ticket, reviewed.clone())
            .unwrap();
        let comparison_ticket = view
            .agent_review
            .as_ref()
            .unwrap()
            .comparison_review
            .as_ref()
            .unwrap()
            .ticket
            .clone();
        assert!(view.firing_review_ticket().is_none());
        scheduler
            .approve_agent_review(&mut view, Some(&primary_ticket))
            .unwrap();
        assert!(view.agent_approval.is_none());
        assert!(
            scheduler
                .approve_swarm_comparison(&mut view, &comparison_ticket, &template("Changed goal"))
                .is_err()
        );
        let _ = scheduler
            .approve_swarm_comparison(&mut view, &comparison_ticket, &reviewed)
            .unwrap();
        assert!(view.agent_approval.is_none());
        let renewed = view.firing_review_ticket().unwrap().to_owned();
        assert_ne!(renewed, primary_ticket);
        let _ = scheduler.update_swarm_comparison_review(
            &mut view,
            &Message::CancelSwarmComparison(comparison_ticket),
        );
        scheduler
            .approve_agent_review(&mut view, Some(&primary_ticket))
            .unwrap();
        assert!(view.agent_approval.is_none());
        scheduler
            .approve_agent_review(&mut view, Some(&renewed))
            .unwrap();
        let approved = view.agent_approval.as_ref().unwrap();
        assert!(
            approved
                .approval
                .comparison
                .as_ref()
                .unwrap()
                .matches(&reviewed)
        );
        assert!(approved.items.is_none());
    }
}
