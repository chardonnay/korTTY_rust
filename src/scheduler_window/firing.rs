//! The model request and approval retained for one scheduled Agent start.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Instant, SystemTime};

use kortty_jobs::RunId;

use crate::ai_manager::SkillUsageReceipt;
use crate::local_ai::Cancel;
use crate::scheduled_agent_review::{self, Phase, Ticket};
use crate::terminal_logging::RedactionSecrets;

use super::agent::proposal::{Item, Prompt};
use super::{Page, SchedulerView, SchedulerWindow, SshCommand, agent};

pub(crate) struct Context {
    pub(crate) prompt: Arc<Prompt>,
    pub(crate) initial_probe: bool,
    pub(crate) shared_secrets: Option<RedactionSecrets>,
    pub(crate) connection_store: PathBuf,
    pub(crate) vault_revision: Option<Weak<()>>,
    pub(crate) _cancel: Option<Cancel>,
    pub(crate) skill_usage: Option<SkillUsageReceipt>,
}

pub(crate) type Reviews = scheduled_agent_review::Reviews<SshCommand, Context, Vec<Item>>;

#[derive(Clone, Copy)]
pub(super) struct ProposalTarget<'a> {
    pub(super) index: usize,
    pub(super) count: usize,
    pub(super) route: &'a agent::Target,
    pub(super) swarm_round: Option<u8>,
    pub(super) initial_probe: bool,
}

pub(super) enum RunProposalView<'a> {
    Generating(ProposalTarget<'a>),
    Awaiting {
        items: &'a [Item],
        directory: &'a str,
        target: ProposalTarget<'a>,
        expires_at: SystemTime,
        ticket: &'a Ticket,
    },
    Authorized(ProposalTarget<'a>),
    Executing(ProposalTarget<'a>),
}

impl<'a> RunProposalView<'a> {
    pub(super) fn target(&self) -> ProposalTarget<'a> {
        match self {
            Self::Generating(target)
            | Self::Authorized(target)
            | Self::Executing(target)
            | Self::Awaiting { target, .. } => *target,
        }
    }
}

impl SchedulerWindow {
    pub(super) fn active_proposals(&self, id: RunId) -> Vec<RunProposalView<'_>> {
        self.firing_reviews
            .for_run(id)
            .filter_map(run_proposal)
            .collect()
    }

    pub(crate) fn can_decide_proposal(&self, view: &SchedulerView, ticket: &Ticket) -> bool {
        view.page == Page::Journal
            && view.selected_run == Some(ticket.run())
            && view.confirm.is_none()
            && view.journal_active_visible(self, ticket.run())
            && self
                .firing_reviews
                .get(ticket, Instant::now())
                .is_some_and(|review| review.phase() == Phase::AwaitingReview)
    }
}

fn run_proposal(
    review: &scheduled_agent_review::Review<SshCommand, Context, Vec<Item>>,
) -> Option<RunProposalView<'_>> {
    let action = &review.request().action;
    let targets = &action.agent.as_ref()?.approval.as_ref()?.targets;
    let index = review.ticket().target_index();
    let target = ProposalTarget {
        index,
        count: targets.len(),
        route: targets.get(index)?,
        swarm_round: (review.context().prompt.is_swarm_iterative()
            && !review.context().initial_probe)
            .then_some(review.ticket().round()),
        initial_probe: review.context().initial_probe,
    };
    Some(match review.phase() {
        Phase::Generating => RunProposalView::Generating(target),
        Phase::AwaitingReview => RunProposalView::Awaiting {
            items: review.proposal()?,
            directory: action.working_directory.as_deref()?,
            target,
            expires_at: SystemTime::now()
                .checked_add(review.deadline()?.saturating_duration_since(Instant::now()))?,
            ticket: review.ticket(),
        },
        Phase::Authorized => RunProposalView::Authorized(target),
        Phase::Executing => RunProposalView::Executing(target),
    })
}
